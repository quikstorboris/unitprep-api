use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::Mutex;

use super::config::DropboxConfig;

#[derive(Debug, thiserror::Error)]
pub enum DropboxError {
    #[error("Dropbox request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Dropbox API returned {status}: {body}")]
    Api { status: u16, body: String },
}

/// One entry from `files/list_folder` -- deliberately minimal (just
/// enough for a future folder picker), not the full Dropbox metadata
/// shape (no size, timestamps, content hash, etc.).
#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    #[serde(rename = ".tag")]
    tag: String,
    pub name: String,
    pub path_display: String,
}

impl Entry {
    pub fn is_folder(&self) -> bool {
        self.tag == "folder"
    }
}

#[derive(Deserialize)]
struct ListFolderResponse {
    entries: Vec<Entry>,
    // Not consumed yet -- see `DropboxClient::list_folder`'s doc comment.
    #[allow(dead_code)]
    has_more: bool,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

/// How much time-left-on-the-clock triggers a proactive refresh rather
/// than risking a request racing the token's real expiry mid-flight.
const REFRESH_SAFETY_MARGIN: Duration = Duration::from_secs(60);

pub struct DropboxClient {
    http: reqwest::Client,
    config: DropboxConfig,
    // Mutex, not RwLock: refreshes happen roughly once per 4 hours, so
    // there is no real read-concurrency to optimize for, and a plain
    // Mutex is simpler to reason about.
    token: Mutex<Option<CachedToken>>,
}

impl DropboxClient {
    pub fn new(config: DropboxConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            config,
            token: Mutex::new(None),
        }
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

        let response = self
            .http
            .post("https://api.dropboxapi.com/oauth2/token")
            .form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", &self.config.refresh_token),
                ("client_id", &self.config.app_key),
                ("client_secret", &self.config.app_secret),
            ])
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            tracing::error!(
                status = status.as_u16(),
                body = %body,
                "Dropbox access token refresh failed"
            );
            return Err(DropboxError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: TokenResponse = serde_json::from_str(&body).map_err(|err| DropboxError::Api {
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

    fn path_root_header(&self) -> String {
        format!(
            "{{\".tag\": \"root\", \"root\": \"{}\"}}",
            self.config.root_namespace_id
        )
    }

    /// Lists one folder, non-recursively. Does not follow pagination
    /// (`has_more`/`list_folder/continue`) -- not needed for the current
    /// QMS Onboarding folder (282 entries, well within a single page)
    /// and left unhandled rather than built speculatively. If a listing
    /// this calls ever silently returns fewer entries than expected,
    /// pagination is the first thing to check.
    pub async fn list_folder(&self, path: &str) -> Result<Vec<Entry>, DropboxError> {
        let access_token = self.access_token().await?;

        let response = self
            .http
            .post("https://api.dropboxapi.com/2/files/list_folder")
            .bearer_auth(access_token)
            .header("Dropbox-API-Path-Root", self.path_root_header())
            .json(&serde_json::json!({ "path": path, "recursive": false }))
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            tracing::error!(
                path = %path,
                status = status.as_u16(),
                body = %body,
                "Dropbox list_folder failed"
            );
            return Err(DropboxError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: ListFolderResponse =
            serde_json::from_str(&body).map_err(|err| DropboxError::Api {
                status: status.as_u16(),
                body: format!("failed to parse list_folder response ({err}): {body}"),
            })?;

        tracing::info!(
            path = %path,
            entry_count = parsed.entries.len(),
            "dropbox list_folder succeeded"
        );

        Ok(parsed.entries)
    }

    // Not called by anything yet -- write-back (reading a customer's
    // existing files into a tool) is planned per the Dropbox integration
    // plan but not wired into any tool yet. Kept rather than deleted:
    // Phase 1 deliberately scoped in read+write support at the client
    // level ahead of any specific caller, since all three tools will
    // need it. See dropbox::client's own #[ignore]d test for list_folder
    // coverage; this and upload below have no equivalent test yet
    // because nothing calls them to have a regression against.
    #[allow(dead_code)]
    pub async fn download(&self, path: &str) -> Result<Vec<u8>, DropboxError> {
        let access_token = self.access_token().await?;

        let response = self
            .http
            .post("https://content.dropboxapi.com/2/files/download")
            .bearer_auth(access_token)
            .header("Dropbox-API-Path-Root", self.path_root_header())
            .header(
                "Dropbox-API-Arg",
                serde_json::json!({ "path": path }).to_string(),
            )
            .send()
            .await?;

        let status = response.status();

        if !status.is_success() {
            let body = response.text().await?;
            tracing::error!(
                path = %path,
                status = status.as_u16(),
                body = %body,
                "Dropbox download failed"
            );
            return Err(DropboxError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let bytes = response.bytes().await?.to_vec();

        tracing::info!(path = %path, byte_count = bytes.len(), "dropbox download succeeded");

        Ok(bytes)
    }

    /// Uploads `bytes` to `path`, overwriting whatever is already there.
    /// Dropbox's other write modes (`add`, with conflict detection via
    /// `update`'s rev parameter) aren't exposed here -- overwrite is the
    /// only policy Phase 1 needs; a caller that needs conflict-aware
    /// writes should get that decided and added when it's actually
    /// wired into a tool, not guessed at now.
    #[allow(dead_code)]
    pub async fn upload(&self, path: &str, bytes: Vec<u8>) -> Result<(), DropboxError> {
        let access_token = self.access_token().await?;
        let byte_count = bytes.len();

        let response = self
            .http
            .post("https://content.dropboxapi.com/2/files/upload")
            .bearer_auth(access_token)
            .header("Dropbox-API-Path-Root", self.path_root_header())
            .header(
                "Dropbox-API-Arg",
                serde_json::json!({ "path": path, "mode": "overwrite" }).to_string(),
            )
            .header("Content-Type", "application/octet-stream")
            .body(bytes)
            .send()
            .await?;

        let status = response.status();

        if !status.is_success() {
            let body = response.text().await?;
            tracing::error!(
                path = %path,
                status = status.as_u16(),
                body = %body,
                "Dropbox upload failed"
            );
            return Err(DropboxError::Api {
                status: status.as_u16(),
                body,
            });
        }

        tracing::info!(path = %path, byte_count, "dropbox upload succeeded");

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real-network test against the actual Dropbox account and QMS
    // Onboarding folder -- no mocking, matching this codebase's existing
    // #[ignore]d real-credential tests (see
    // auth::authenticated_user's and auth::roles's DB-backed ones).
    // Requires .env.local to hold real DROPBOX_* values. Run with:
    //   cargo test --ignored dropbox
    #[tokio::test]
    #[ignore]
    async fn lists_the_real_qms_onboarding_folder() {
        let _ = dotenvy::from_filename(".env.local");

        let config = DropboxConfig::from_env().expect(
            "DROPBOX_* env vars must be set in .env.local to run this ignored test",
        );
        let root_path = config.root_path.clone();
        let client = DropboxClient::new(config);

        let entries = client
            .list_folder(&root_path)
            .await
            .expect("list_folder should succeed against the real QMS Onboarding folder");

        assert!(
            entries.len() > 200,
            "expected roughly 282 customer subfolders, got {}",
            entries.len()
        );
        assert!(
            entries.iter().any(|e| e.is_folder() && e.name == "Papa Ducks"),
            "expected to find the known 'Papa Ducks' subfolder"
        );
    }
}
