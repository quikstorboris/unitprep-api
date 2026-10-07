use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

use super::config::DropboxConfig;
use crate::integrations::http::{
    client_builder, send_with_retry, truncate_for_log, RetryPolicy, MAX_LOGGED_BODY_BYTES,
};

mod dto;
mod files;
mod folders;
#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod tests;

pub use dto::DropboxError;
use dto::TokenResponse;

struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

/// How much time-left-on-the-clock triggers a proactive refresh rather
/// than risking a request racing the token's real expiry mid-flight.
const REFRESH_SAFETY_MARGIN: Duration = Duration::from_secs(60);

/// Per-request ceiling for the OAuth token refresh -- tighter than the
/// shared default because it runs under the token mutex.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

/// Per-request ceiling for moving file contents (download/upload), which
/// can legitimately outlast the shared 30 s default.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(300);

/// Where Dropbox's two hosts live. Production code only ever uses
/// [`Endpoints::production`]; the override constructor
/// (`DropboxClient::with_endpoints`) is compiled ONLY into test builds, so
/// no configuration, environment variable or request can ever redirect the
/// refresh token and app secret to another host in a release build -- the
/// same rule as the ClickUp client's test seam.
struct Endpoints {
    /// OAuth (`/oauth2/token`) and every JSON RPC call.
    api: String,
    /// File content (`/2/files/download`, `/2/files/upload`).
    content: String,
}

impl Endpoints {
    fn production() -> Self {
        Self {
            api: "https://api.dropboxapi.com".to_string(),
            content: "https://content.dropboxapi.com".to_string(),
        }
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}{path}", self.api)
    }

    fn content_url(&self, path: &str) -> String {
        format!("{}{path}", self.content)
    }
}

pub struct DropboxClient {
    http: reqwest::Client,
    config: DropboxConfig,
    endpoints: Endpoints,
    // Mutex, not RwLock: refreshes happen roughly once per 4 hours, so
    // there is no real read-concurrency to optimize for, and a plain
    // Mutex is simpler to reason about.
    token: Mutex<Option<CachedToken>>,
}

impl DropboxClient {
    pub fn new(config: DropboxConfig) -> Self {
        Self {
            http: client_builder()
                .build()
                .expect("a reqwest client with only timeouts configured must build"),
            config,
            endpoints: Endpoints::production(),
            token: Mutex::new(None),
        }
    }

    /// Test-only: aim the client at a local mock Dropbox. Compiled out of
    /// release builds -- see [`Endpoints`].
    #[cfg(test)]
    fn with_endpoints(config: DropboxConfig, api: String, content: String) -> Self {
        let mut client = Self::new(config);
        client.endpoints = Endpoints { api, content };
        client
    }

    /// The app-level path boundary described in this module's parent doc
    /// comment -- Dropbox itself enforces nothing narrower than "this
    /// account". Callers that expose browsing to end users (see
    /// `api::dropbox_browse`) must check any caller-supplied path against
    /// this before calling `list_folder`/`download`/`upload`.
    pub fn root_path(&self) -> &str {
        &self.config.root_path
    }

    /// Returns a live access token, refreshing it first if it's missing
    /// or within `REFRESH_SAFETY_MARGIN` of expiring.
    async fn access_token(&self) -> Result<String, DropboxError> {
        let mut cached = self.token.lock().await;

        if let Some(token) = cached.as_ref() {
            if token.expires_at > Instant::now() + REFRESH_SAFETY_MARGIN {
                return Ok(token.access_token.clone());
            }
        }

        // This runs while holding the token mutex, so a hung refresh used
        // to stall every other Dropbox call in the process. It now has a
        // tight per-request timeout and at most one retry, so the lock is
        // held for a bounded time (about 2 x REFRESH_TIMEOUT worst case).
        // A refresh is a pure token exchange, safe to repeat.
        let response = send_with_retry(RetryPolicy::QUICK, || {
            self.http
                .post(self.endpoints.api_url("/oauth2/token"))
                .timeout(REFRESH_TIMEOUT)
                .form(&[
                    ("grant_type", "refresh_token"),
                    ("refresh_token", &self.config.refresh_token),
                    ("client_id", &self.config.app_key),
                    ("client_secret", &self.config.app_secret),
                ])
        })
        .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            tracing::error!(
                status = status.as_u16(),
                body = %truncate_for_log(&body, MAX_LOGGED_BODY_BYTES),
                "Dropbox access token refresh failed"
            );
            return Err(DropboxError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: TokenResponse =
            serde_json::from_str(&body).map_err(|err| DropboxError::Api {
                status: status.as_u16(),
                body: format!("failed to parse token response ({err}): {body}"),
            })?;

        let access_token = parsed.access_token.clone();

        // No user info here by design -- this token is a single
        // app-wide credential shared across every caller/request, not
        // scoped to whichever staff member's action happened to trigger
        // the refresh. Callers that act on behalf of a specific user
        // (see api::dropbox_browse) are the right place to log that
        // user's identity alongside the *operation* they asked for.
        tracing::info!(
            expires_in_secs = parsed.expires_in,
            "refreshed Dropbox access token"
        );

        *cached = Some(CachedToken {
            access_token: parsed.access_token,
            expires_at: Instant::now() + Duration::from_secs(parsed.expires_in),
        });

        Ok(access_token)
    }

    /// Forgets the cached access token so the next `access_token()` call
    /// refreshes it. Used when Dropbox says the token in hand is no good.
    async fn invalidate_token(&self) {
        *self.token.lock().await = None;
    }

    /// Sends the request `build` produces with a live access token, with
    /// transient-failure retries per `policy`. If Dropbox answers 401 the
    /// cached token is dropped, a fresh one is fetched, and the request is
    /// sent once more -- a token can expire (or be revoked) between the
    /// cache check and the call, and that should not surface to a user as
    /// a failure. A second 401 is returned to the caller as-is.
    async fn send_authed<F>(
        &self,
        policy: RetryPolicy,
        build: F,
    ) -> Result<reqwest::Response, DropboxError>
    where
        F: Fn(&str) -> reqwest::RequestBuilder,
    {
        let token = self.access_token().await?;
        let response = send_with_retry(policy, || build(&token)).await?;

        if response.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(response);
        }

        tracing::warn!("Dropbox returned 401; refreshing the access token and retrying once");
        self.invalidate_token().await;
        let token = self.access_token().await?;

        Ok(send_with_retry(policy, || build(&token)).await?)
    }

    fn path_root_header(&self) -> String {
        format!(
            "{{\".tag\": \"root\", \"root\": \"{}\"}}",
            self.config.root_namespace_id
        )
    }

    /// POSTs a JSON body to a Dropbox API endpoint with a live token (and the
    /// app's path root, when the call is namespace-relative), retrying
    /// transient failures, and hands back the status and body text. Every
    /// JSON call goes through here, so the token header, the path-root
    /// header, the 401 refresh-and-retry and the error mapping live in one
    /// place; each caller decides what a non-success status means for it.
    async fn rpc(
        &self,
        endpoint: &str,
        body: &serde_json::Value,
        with_path_root: bool,
    ) -> Result<Reply, DropboxError> {
        let response = self
            .send_authed(RetryPolicy::STANDARD, |token| {
                let request = self
                    .http
                    .post(self.endpoints.api_url(endpoint))
                    .bearer_auth(token);
                let request = if with_path_root {
                    request.header("Dropbox-API-Path-Root", self.path_root_header())
                } else {
                    request
                };
                request.json(body)
            })
            .await?;

        let status = response.status();
        let body = response.text().await?;

        Ok(Reply { status, body })
    }
}

/// A Dropbox JSON answer, read to the end: its status and body text.
pub(super) struct Reply {
    pub(super) status: reqwest::StatusCode,
    pub(super) body: String,
}

impl Reply {
    pub(super) fn is_success(&self) -> bool {
        self.status.is_success()
    }

    /// This reply as the error a caller returns for an unwelcome status.
    pub(super) fn into_error(self) -> DropboxError {
        DropboxError::Api {
            status: self.status.as_u16(),
            body: self.body,
        }
    }

    /// Parses the body as `T`; a body that does not parse is an API error
    /// that carries the text, so the cause is visible in the logs.
    pub(super) fn parse<T: DeserializeOwned>(&self, what: &str) -> Result<T, DropboxError> {
        serde_json::from_str(&self.body).map_err(|err| DropboxError::Api {
            status: self.status.as_u16(),
            body: format!("failed to parse {what} ({err}): {}", self.body),
        })
    }
}
