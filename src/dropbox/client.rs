use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::Mutex;

use super::config::DropboxConfig;
use crate::integrations::http::{
    client_builder, send_with_retry, truncate_for_log, RetryPolicy, MAX_LOGGED_BODY_BYTES,
};

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

    #[cfg(test)]
    fn test_folder(name: &str, path_display: &str) -> Self {
        Self {
            tag: "folder".to_string(),
            name: name.to_string(),
            path_display: path_display.to_string(),
        }
    }
}

#[derive(Deserialize)]
struct ListFolderResponse {
    entries: Vec<Entry>,
    /// More entries exist beyond this page: fetch them with
    /// `files/list_folder/continue` and `cursor`.
    has_more: bool,
    #[serde(default)]
    cursor: String,
}

/// A safety valve against a cursor that never ends, not a limit anyone is
/// expected to reach: Dropbox pages hold up to 2,000 entries, so this is
/// hundreds of thousands of entries. Exceeding it is an error -- silently
/// returning a truncated listing is exactly the bug pagination fixes.
const MAX_LIST_FOLDER_PAGES: usize = 100;

/// `files/search_v2`'s response shape is unrelated to `list_folder`'s
/// (a `matches` array of match wrappers, not a flat `entries` array),
/// but each match's inner `metadata.metadata` object has exactly the
/// same `.tag`/`name`/`path_display` fields `Entry` already parses --
/// reused as-is rather than duplicating a second near-identical struct
/// (serde ignores the extra fields search results carry, like
/// `match_type`/`highlight_spans`, since `Entry` never named them).
#[derive(Deserialize)]
struct SearchV2Response {
    matches: Vec<SearchV2Match>,
}

#[derive(Deserialize)]
struct SearchV2Match {
    metadata: SearchV2MatchMetadata,
}

#[derive(Deserialize)]
struct SearchV2MatchMetadata {
    metadata: Entry,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

/// `sharing/get_shared_link_metadata`'s response -- deliberately just
/// `.tag`/`id`, not the fuller shape (name, link_permissions,
/// team_member_info, ...) `resolve_shared_link` doesn't need. `id` is
/// the one field worth anything here: a Dropbox-wide object identifier
/// that resolves to a real path under THIS account's own namespace via
/// `files/get_metadata`, even when this response's own path_lower would
/// be absent (see `resolve_shared_link`'s own doc comment).
#[derive(Deserialize)]
struct SharedLinkMetadataResponse {
    #[serde(rename = ".tag")]
    tag: String,
    id: String,
}

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

/// `DropboxClient::find_facility_folder`'s own picking logic, pulled out
/// as a pure function so it's testable without a real network call --
/// see that method's own doc comment for why this is exact-match-only.
///
/// **A "there's only one candidate, it must be it" fallback was tried
/// and reverted the same day** (2026-09-04): searching Dropbox for
/// Sand-Sto's own real facility name ("Sand-Sto Climate Controlled
/// Storage") returned exactly one folder candidate -- and it was
/// `sand_sto_climate_control_storage_decrypt`, an unrelated folder
/// nowhere near the real one ("Sand-Sto Storage", found only by manual
/// browsing). Dropbox's own search ranking is not reliable enough to
/// assume "the only result" means "the right result" -- a wrong guess
/// here silently points a user at an unrelated (and, going by that
/// name, possibly sensitive) folder, which is a worse outcome than
/// finding nothing and falling back to browsing from the root.
fn pick_facility_folder(folders: Vec<Entry>, facility_name: &str) -> Option<Entry> {
    folders.into_iter().find(|f| f.name == facility_name)
}

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

    /// Lists one folder, non-recursively, following Dropbox's pagination
    /// (`has_more` / `files/list_folder/continue`) until every entry has
    /// been returned.
    ///
    /// This used to return only the first page and silently ignore
    /// `has_more` -- fine for the QMS Onboarding folder when it held 282
    /// entries, wrong the moment any folder passed a page (~2,000 entries):
    /// the folder picker and the Dedup folder scan would simply have shown
    /// fewer files than the folder holds, with no error anywhere.
    pub async fn list_folder(&self, path: &str) -> Result<Vec<Entry>, DropboxError> {
        let mut entries = Vec::new();
        let mut page = self
            .list_folder_page(
                "/2/files/list_folder",
                path,
                serde_json::json!({ "path": path, "recursive": false }),
            )
            .await?;
        let mut pages = 1;

        loop {
            entries.append(&mut page.entries);

            if !page.has_more {
                break;
            }
            if pages >= MAX_LIST_FOLDER_PAGES {
                return Err(DropboxError::Api {
                    status: 200,
                    body: format!(
                        "list_folder for {path} still had more entries after {MAX_LIST_FOLDER_PAGES} pages"
                    ),
                });
            }

            page = self
                .list_folder_page(
                    "/2/files/list_folder/continue",
                    path,
                    serde_json::json!({ "cursor": page.cursor }),
                )
                .await?;
            pages += 1;
        }

        tracing::info!(
            path = %path,
            entry_count = entries.len(),
            pages,
            "dropbox list_folder succeeded"
        );

        Ok(entries)
    }

    /// One page of a folder listing: the first call (`/2/files/list_folder`)
    /// or a continuation (`/2/files/list_folder/continue`).
    async fn list_folder_page(
        &self,
        endpoint: &str,
        path: &str,
        body: serde_json::Value,
    ) -> Result<ListFolderResponse, DropboxError> {
        let response = self
            .send_authed(RetryPolicy::STANDARD, |token| {
                self.http
                    .post(self.endpoints.api_url(endpoint))
                    .bearer_auth(token)
                    .header("Dropbox-API-Path-Root", self.path_root_header())
                    .json(&body)
            })
            .await?;

        let status = response.status();
        let text = response.text().await?;

        if !status.is_success() {
            tracing::error!(
                path = %path,
                endpoint,
                status = status.as_u16(),
                body = %truncate_for_log(&text, MAX_LOGGED_BODY_BYTES),
                "Dropbox list_folder failed"
            );
            return Err(DropboxError::Api {
                status: status.as_u16(),
                body: text,
            });
        }

        serde_json::from_str(&text).map_err(|err| DropboxError::Api {
            status: status.as_u16(),
            body: format!("failed to parse list_folder response ({err}): {text}"),
        })
    }

    /// Searches folder names recursively under the configured root --
    /// unlike `list_folder`, this needs no caller-supplied path to
    /// validate against the root boundary at all, since the search is
    /// always scoped to `self.config.root_path` by construction.
    ///
    /// Dropbox's search has no "folders only" option: a query matches
    /// file names too, and there is no request parameter to exclude
    /// them. Filtering to folders happens here, after the fact, on
    /// whatever Dropbox returns. `max_results: 100` is deliberately
    /// generous rather than Dropbox's own default (10) -- a facility
    /// folder can easily be outranked by a dozen files whose names also
    /// contain the search term (rent rolls, unit lists, templates), so
    /// asking for too few results risks filtering down to nothing even
    /// though a real folder match exists further down Dropbox's own
    /// ranking. No pagination (`search/continue_v2`) -- not needed for
    /// a facility-name-shaped query (verified against the real QMS
    /// Onboarding folder: two- and three-word queries both returned
    /// every match in one page), and a query broad enough to need it is
    /// arguably not a useful facility search anyway.
    pub async fn search_folders(&self, query: &str) -> Result<Vec<Entry>, DropboxError> {
        let response = self
            .send_authed(RetryPolicy::STANDARD, |token| {
                self.http
                    .post(self.endpoints.api_url("/2/files/search_v2"))
                    .bearer_auth(token)
                    .header("Dropbox-API-Path-Root", self.path_root_header())
                    .json(&serde_json::json!({
                        "query": query,
                        "options": {
                            "path": self.config.root_path,
                            "max_results": 100,
                            "filename_only": true,
                        },
                    }))
            })
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            tracing::error!(
                query = %query,
                status = status.as_u16(),
                body = %truncate_for_log(&body, MAX_LOGGED_BODY_BYTES),
                "Dropbox search failed"
            );
            return Err(DropboxError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: SearchV2Response =
            serde_json::from_str(&body).map_err(|err| DropboxError::Api {
                status: status.as_u16(),
                body: format!("failed to parse search response ({err}): {body}"),
            })?;

        let folders: Vec<Entry> = parsed
            .matches
            .into_iter()
            .map(|m| m.metadata.metadata)
            .filter(Entry::is_folder)
            .collect();

        tracing::info!(
            query = %query,
            folder_count = folders.len(),
            "dropbox search_folders succeeded"
        );

        Ok(folders)
    }

    /// Resolves a Process-Street-captured Dropbox shared-link URL (the
    /// `https://www.dropbox.com/scl/fo/...` links `clients.facilities.
    /// dropbox_folder_url` stores, filled in by hand into PS's own
    /// `Facility_Onboarding_folder_URL:` field) to the real, writable
    /// path it corresponds to in THIS account's own namespace -- the
    /// reliable, primary way to find a facility's own folder.
    /// `find_facility_folder` below is the fallback for when this
    /// returns `None` (no link at all, or the link itself is stale).
    ///
    /// **The two-step bridge this needs, confirmed live 2026-09-04**:
    /// such a link is typically shared by an individual staff member's
    /// own Dropbox account (Highway 20's real link: team "KoBre", member
    /// "Kyle Murakami") -- not necessarily the same Dropbox Business
    /// team this app's own token belongs to ("QS Fileserver"). Calling
    /// `sharing/get_shared_link_metadata` alone on such a link comes back
    /// with real name/type metadata but no `path_lower` (no filesystem-
    /// level view from this account's own perspective) -- not enough to
    /// list, create a folder in, or upload to it directly. But its `id`
    /// field is a Dropbox-wide, account-independent object identifier;
    /// calling `files/get_metadata` on that same id, under THIS account's
    /// own `Dropbox-API-Path-Root`, resolves to a real `path_display` in
    /// THIS account's own namespace whenever this account also has
    /// access to the same underlying folder -- which it does for every
    /// real facility folder in the shared "QMS Onboarding" tree, proven
    /// against both Highway 20's own link and, critically, Sand-Sto's
    /// (the real case where the facility's OO name -- "Sand-Sto Climate
    /// Controlled Storage" -- doesn't match its actual Dropbox folder
    /// name, "Sand-Sto Storage" -- this still resolves correctly, unlike
    /// a name search, since it never depends on the name matching at
    /// all).
    ///
    /// Degrades to `Ok(None)` (not an error) for any failure along the
    /// way -- a revoked/expired link, a link this account genuinely
    /// can't reach, or a link that resolves to a file rather than a
    /// folder -- since a broken link is a normal real-world occurrence
    /// here, not a system fault; the caller falls back to
    /// `find_facility_folder`. A transport-level failure (`?` on
    /// `access_token()`) still propagates as a real error.
    pub async fn resolve_shared_link(&self, url: &str) -> Result<Option<Entry>, DropboxError> {
        let link_response = self
            .send_authed(RetryPolicy::STANDARD, |token| {
                self.http
                    .post(
                        self.endpoints
                            .api_url("/2/sharing/get_shared_link_metadata"),
                    )
                    .bearer_auth(token)
                    .json(&serde_json::json!({ "url": url }))
            })
            .await?;

        let status = link_response.status();
        let body = link_response.text().await?;

        if !status.is_success() {
            tracing::warn!(url = %url, status = status.as_u16(), body = %truncate_for_log(&body, MAX_LOGGED_BODY_BYTES), "Dropbox shared-link resolution failed, falling back to name search");
            return Ok(None);
        }

        let link_metadata: SharedLinkMetadataResponse = match serde_json::from_str(&body) {
            Ok(parsed) => parsed,
            Err(err) => {
                tracing::warn!(url = %url, error = %err, body = %truncate_for_log(&body, MAX_LOGGED_BODY_BYTES), "failed to parse shared-link metadata response, falling back to name search");
                return Ok(None);
            }
        };

        if link_metadata.tag != "folder" {
            tracing::warn!(url = %url, tag = %link_metadata.tag, "shared link does not point at a folder, falling back to name search");
            return Ok(None);
        }

        let metadata_response = self
            .send_authed(RetryPolicy::STANDARD, |token| {
                self.http
                    .post(self.endpoints.api_url("/2/files/get_metadata"))
                    .bearer_auth(token)
                    .header("Dropbox-API-Path-Root", self.path_root_header())
                    .json(&serde_json::json!({ "path": link_metadata.id }))
            })
            .await?;

        let status = metadata_response.status();
        let body = metadata_response.text().await?;

        if !status.is_success() {
            tracing::warn!(url = %url, id = %link_metadata.id, status = status.as_u16(), body = %truncate_for_log(&body, MAX_LOGGED_BODY_BYTES), "Dropbox get_metadata by shared-link id failed, falling back to name search");
            return Ok(None);
        }

        match serde_json::from_str::<Entry>(&body) {
            Ok(entry) => {
                tracing::info!(url = %url, path = %entry.path_display, "dropbox shared link resolved to a real path via its object id");
                Ok(Some(entry))
            }
            Err(err) => {
                tracing::warn!(url = %url, error = %err, body = %truncate_for_log(&body, MAX_LOGGED_BODY_BYTES), "failed to parse get_metadata response, falling back to name search");
                Ok(None)
            }
        }
    }

    /// Finds a facility's own Dropbox folder by name under this app's
    /// connected root -- the fallback `resolve_shared_link` above uses
    /// when a facility has no captured link at all, or that link fails
    /// to resolve. Exact name match only (a facility name search can
    /// otherwise surface files that merely mention it, e.g. "Highway 20
    /// Self Storage Unit Coverages.csv", which `search_folders`'s own
    /// folder-only filter already drops, but a same-named sub-item one
    /// level down could still slip through). `None` when nothing matches
    /// exactly -- this fallback path has no reliable way to handle a
    /// facility whose OO name doesn't match its real Dropbox folder name
    /// (that's exactly what `resolve_shared_link` is for); see
    /// `pick_facility_folder`'s own doc comment for why a same-day "just
    /// take the only candidate" fallback was tried and reverted here --
    /// it isn't safe to assume Dropbox's search ranking narrowing to one
    /// result means that result is right.
    pub async fn find_facility_folder(
        &self,
        facility_name: &str,
    ) -> Result<Option<Entry>, DropboxError> {
        let folders = self.search_folders(facility_name).await?;
        Ok(pick_facility_folder(folders, facility_name))
    }

    /// Creates `path` as a folder if it doesn't already exist -- the
    /// `Duplicate Check` subfolder this app auto-creates next to wherever
    /// a source file was imported from is the one real caller. A
    /// `path/conflict/folder` error (the folder is already there) is
    /// treated as success, since the caller's actual goal -- "this folder
    /// exists" -- is already satisfied; every other error propagates.
    pub async fn create_folder_if_missing(&self, path: &str) -> Result<(), DropboxError> {
        // Safe to retry: a repeat of a create that actually landed comes
        // back as the 409 "already exists" handled just below.
        let response = self
            .send_authed(RetryPolicy::STANDARD, |token| {
                self.http
                    .post(self.endpoints.api_url("/2/files/create_folder_v2"))
                    .bearer_auth(token)
                    .header("Dropbox-API-Path-Root", self.path_root_header())
                    .json(&serde_json::json!({ "path": path }))
            })
            .await?;

        let status = response.status();

        if status.is_success() {
            tracing::info!(path = %path, "dropbox create_folder_v2 succeeded");
            return Ok(());
        }

        let body = response.text().await?;

        // Dropbox reports "folder already exists" as a 409 carrying a
        // structured error tag in the body, not a distinct HTTP status.
        if status.as_u16() == 409 && body.contains("path/conflict/folder") {
            tracing::info!(path = %path, "dropbox create_folder_v2: folder already exists");
            return Ok(());
        }

        tracing::error!(
            path = %path,
            status = status.as_u16(),
            body = %truncate_for_log(&body, MAX_LOGGED_BODY_BYTES),
            "Dropbox create_folder_v2 failed"
        );
        Err(DropboxError::Api {
            status: status.as_u16(),
            body,
        })
    }

    // Used by api::dedup's Dropbox-import handlers
    // (download_as_uploaded_file). See dropbox::client's own #[ignore]d
    // test for list_folder coverage; this and upload below have no
    // equivalent test yet since the real network call isn't something a
    // fast unit test should exercise -- see api::dedup's own no-network
    // rejection tests for what actually is covered.
    pub async fn download(&self, path: &str) -> Result<Vec<u8>, DropboxError> {
        let response = self
            .send_authed(RetryPolicy::STANDARD, |token| {
                self.http
                    .post(self.endpoints.content_url("/2/files/download"))
                    .timeout(TRANSFER_TIMEOUT)
                    .bearer_auth(token)
                    .header("Dropbox-API-Path-Root", self.path_root_header())
                    .header(
                        "Dropbox-API-Arg",
                        serde_json::json!({ "path": path }).to_string(),
                    )
            })
            .await?;

        let status = response.status();

        if !status.is_success() {
            let body = response.text().await?;
            tracing::error!(
                path = %path,
                status = status.as_u16(),
                body = %truncate_for_log(&body, MAX_LOGGED_BODY_BYTES),
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
    /// only policy the one caller (api::dedup's export_to_dropbox) needs;
    /// the frontend guards against silent clobbering itself by always
    /// generating a timestamped filename, not by asking Dropbox to
    /// detect a conflict.
    pub async fn upload(&self, path: &str, bytes: Vec<u8>) -> Result<(), DropboxError> {
        let access_token = self.access_token().await?;
        let byte_count = bytes.len();

        let response = self
            .http
            .post(self.endpoints.content_url("/2/files/upload"))
            // Deliberately NOT retried (no `send_authed`): the body is
            // moved into the request, and an overwrite upload that may
            // already have landed is not something to repeat blindly.
            .timeout(TRANSFER_TIMEOUT)
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
                body = %truncate_for_log(&body, MAX_LOGGED_BODY_BYTES),
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

    /// A Dropbox link to the file at `path`, reusing the one that already
    /// exists (a second request for the same file is answered by Dropbox
    /// with `shared_link_already_exists`). No audience settings are sent,
    /// so the link takes the account's own default (a team-only link in
    /// a team account). Needs the app's `sharing.write` scope; without it
    /// Dropbox answers with an error and the caller falls back to a
    /// plain web path.
    pub async fn shared_link(&self, path: &str) -> Result<String, DropboxError> {
        let response = self
            .send_authed(RetryPolicy::STANDARD, |token| {
                self.http
                    .post(
                        self.endpoints
                            .api_url("/2/sharing/create_shared_link_with_settings"),
                    )
                    .bearer_auth(token)
                    .header("Dropbox-API-Path-Root", self.path_root_header())
                    .json(&serde_json::json!({ "path": path }))
            })
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if status.is_success() {
            return link_url(&body);
        }

        if status.as_u16() == 409 && body.contains("shared_link_already_exists") {
            return self.existing_shared_link(path).await;
        }

        Err(DropboxError::Api {
            status: status.as_u16(),
            body,
        })
    }

    async fn existing_shared_link(&self, path: &str) -> Result<String, DropboxError> {
        let response = self
            .send_authed(RetryPolicy::STANDARD, |token| {
                self.http
                    .post(self.endpoints.api_url("/2/sharing/list_shared_links"))
                    .bearer_auth(token)
                    .header("Dropbox-API-Path-Root", self.path_root_header())
                    .json(&serde_json::json!({ "path": path, "direct_only": true }))
            })
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            return Err(DropboxError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let parsed: serde_json::Value =
            serde_json::from_str(&body).map_err(|err| DropboxError::Api {
                status: status.as_u16(),
                body: format!("failed to parse shared-link list ({err}): {body}"),
            })?;

        parsed["links"][0]["url"]
            .as_str()
            .map(str::to_string)
            .ok_or(DropboxError::Api {
                status: status.as_u16(),
                body: "no shared link in the list response".to_string(),
            })
    }
}

/// The `url` of a `create_shared_link_with_settings` response.
fn link_url(body: &str) -> Result<String, DropboxError> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|parsed| parsed["url"].as_str().map(str::to_string))
        .ok_or_else(|| DropboxError::Api {
            status: 200,
            body: format!("no url in the shared-link response: {body}"),
        })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    /// A loopback Dropbox that records the Authorization header it saw and
    /// answers the first request 503, later ones 200.
    async fn spawn_flaky_dropbox() -> (String, Arc<AtomicUsize>, Arc<std::sync::Mutex<Vec<String>>>)
    {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen_auth = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let (counter, seen) = (calls.clone(), seen_auth.clone());

        let app = axum::Router::new().route(
            "/",
            axum::routing::post(move |headers: axum::http::HeaderMap| {
                let (counter, seen) = (counter.clone(), seen.clone());
                async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    seen.lock().unwrap().push(auth);

                    if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                        axum::http::StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        axum::http::StatusCode::OK
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (url, calls, seen_auth)
    }

    #[tokio::test]
    async fn send_authed_sends_the_cached_token_and_retries_a_transient_failure() {
        let (url, calls, seen_auth) = spawn_flaky_dropbox().await;
        let client = DropboxClient::new(DropboxConfig {
            app_key: "k".into(),
            app_secret: "s".into(),
            refresh_token: "r".into(),
            root_namespace_id: "1".into(),
            root_path: "/".into(),
        });
        // A live cached token, so no real OAuth refresh is attempted.
        *client.token.lock().await = Some(CachedToken {
            access_token: "cached-token".into(),
            expires_at: Instant::now() + Duration::from_secs(3600),
        });

        let response = client
            .send_authed(RetryPolicy::STANDARD, |token| {
                client.http.post(&url).bearer_auth(token)
            })
            .await
            .expect("the retry must succeed");

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "one 503, then the retry");
        assert_eq!(
            *seen_auth.lock().unwrap(),
            vec!["Bearer cached-token".to_string(); 2],
            "both attempts must carry the cached bearer token"
        );
    }

    // ---- a hermetic mock Dropbox (axum on loopback) -----------------------

    use std::collections::VecDeque;

    #[derive(Clone, Debug)]
    struct Recorded {
        path: String,
        authorization: String,
        path_root: String,
        body: serde_json::Value,
    }

    #[derive(Default)]
    struct MockDropbox {
        requests: std::sync::Mutex<Vec<Recorded>>,
        list_pages: std::sync::Mutex<VecDeque<serde_json::Value>>,
        /// Bearer tokens the mock answers with 401 (an expired/revoked token).
        reject_tokens: std::sync::Mutex<Vec<String>>,
        token_calls: AtomicUsize,
    }

    async fn mock_handler(
        axum::extract::State(mock): axum::extract::State<Arc<MockDropbox>>,
        uri: axum::http::Uri,
        headers: axum::http::HeaderMap,
        body: String,
    ) -> (axum::http::StatusCode, String) {
        let path = uri.path().to_string();

        if path == "/oauth2/token" {
            let n = mock.token_calls.fetch_add(1, Ordering::SeqCst) + 1;
            return (
                axum::http::StatusCode::OK,
                serde_json::json!({ "access_token": format!("T{n}"), "expires_in": 14400 })
                    .to_string(),
            );
        }

        let header = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string()
        };
        let authorization = header("authorization");
        mock.requests.lock().unwrap().push(Recorded {
            path,
            authorization: authorization.clone(),
            path_root: header("dropbox-api-path-root"),
            body: serde_json::from_str(&body).unwrap_or(serde_json::Value::Null),
        });

        let rejected = mock
            .reject_tokens
            .lock()
            .unwrap()
            .iter()
            .any(|t| authorization == format!("Bearer {t}"));
        if rejected {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                r#"{"error_summary": "expired_access_token/"}"#.to_string(),
            );
        }

        match mock.list_pages.lock().unwrap().pop_front() {
            Some(page) => (axum::http::StatusCode::OK, page.to_string()),
            None => (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "mock ran out of pages".to_string(),
            ),
        }
    }

    async fn spawn_mock(mock: Arc<MockDropbox>) -> DropboxClient {
        let app = axum::Router::new().fallback(mock_handler).with_state(mock);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        DropboxClient::with_endpoints(
            DropboxConfig {
                app_key: "k".into(),
                app_secret: "s".into(),
                refresh_token: "r".into(),
                root_namespace_id: "ns1".into(),
                root_path: "/Root".into(),
            },
            base.clone(),
            base,
        )
    }

    fn entry(name: &str) -> serde_json::Value {
        serde_json::json!({ ".tag": "folder", "name": name, "path_display": format!("/Root/{name}") })
    }

    fn page(names: &[&str], has_more: bool, cursor: &str) -> serde_json::Value {
        serde_json::json!({
            "entries": names.iter().map(|n| entry(n)).collect::<Vec<_>>(),
            "has_more": has_more,
            "cursor": cursor,
        })
    }

    #[tokio::test]
    async fn list_folder_follows_pagination_until_has_more_is_false() {
        let mock = Arc::new(MockDropbox::default());
        mock.list_pages.lock().unwrap().extend([
            page(&["a", "b"], true, "c1"),
            page(&["c"], true, "c2"),
            page(&["d"], false, "c3"),
        ]);
        let client = spawn_mock(mock.clone()).await;

        let entries = client.list_folder("/Root").await.unwrap();

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c", "d"], "every page, in order");

        let requests = mock.requests.lock().unwrap().clone();
        let paths: Vec<&str> = requests.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "/2/files/list_folder",
                "/2/files/list_folder/continue",
                "/2/files/list_folder/continue"
            ]
        );
        assert_eq!(requests[0].body["path"], "/Root");
        assert_eq!(requests[0].body["recursive"], false);
        assert_eq!(requests[1].body["cursor"], "c1");
        assert_eq!(requests[2].body["cursor"], "c2");
        assert!(
            requests.iter().all(|r| r.path_root.contains("ns1")),
            "every page must carry the namespace root header"
        );
    }

    #[tokio::test]
    async fn list_folder_with_one_page_makes_exactly_one_request() {
        let mock = Arc::new(MockDropbox::default());
        mock.list_pages
            .lock()
            .unwrap()
            .push_back(page(&["only"], false, "c1"));
        let client = spawn_mock(mock.clone()).await;

        let entries = client.list_folder("/Root").await.unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(mock.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_401_refreshes_the_token_and_retries_once_with_the_fresh_one() {
        let mock = Arc::new(MockDropbox::default());
        // The first token the mock mints (T1) is "expired" by the time it is used.
        mock.reject_tokens.lock().unwrap().push("T1".to_string());
        mock.list_pages
            .lock()
            .unwrap()
            .push_back(page(&["x"], false, "c"));
        let client = spawn_mock(mock.clone()).await;

        let entries = client.list_folder("/Root").await.unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(
            mock.token_calls.load(Ordering::SeqCst),
            2,
            "one refresh to get T1, one more after its 401"
        );
        let auths: Vec<String> = mock
            .requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.authorization.clone())
            .collect();
        assert_eq!(auths, vec!["Bearer T1", "Bearer T2"]);
    }

    #[tokio::test]
    async fn a_second_401_is_returned_as_an_error_not_retried_forever() {
        let mock = Arc::new(MockDropbox::default());
        mock.reject_tokens.lock().unwrap().extend([
            "T1".to_string(),
            "T2".to_string(),
            "T3".to_string(),
        ]);
        let client = spawn_mock(mock.clone()).await;

        let err = client.list_folder("/Root").await.unwrap_err();

        assert!(matches!(err, DropboxError::Api { status: 401, .. }));
        assert_eq!(
            mock.token_calls.load(Ordering::SeqCst),
            2,
            "exactly one refresh-and-retry, then give up"
        );
        assert_eq!(mock.requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn picks_the_exact_name_match_over_any_other_candidate() {
        let folders = vec![
            Entry::test_folder("Sand-Sto Storage", "/qms onboarding/sand-sto storage"),
            Entry::test_folder(
                "Sand-Sto Climate Controlled Storage",
                "/qms onboarding/sand-sto climate controlled storage",
            ),
        ];

        let picked = pick_facility_folder(folders, "Sand-Sto Climate Controlled Storage")
            .expect("an exact match exists and must be picked");

        assert_eq!(picked.name, "Sand-Sto Climate Controlled Storage");
    }

    // The real Sand-Sto case (2026-09-04): OO's own facility name
    // ("Sand-Sto Climate Controlled Storage") doesn't match the real
    // Dropbox folder someone actually created for it ("Sand-Sto
    // Storage") -- a single non-exact candidate must NOT be silently
    // picked (see `pick_facility_folder`'s own doc comment for why a
    // same-day fallback attempt here was reverted: Dropbox's search
    // returned exactly one result for this exact real query and it was
    // an unrelated folder, not this one).
    #[test]
    fn picks_nothing_when_a_single_candidate_does_not_match_exactly() {
        let folders = vec![Entry::test_folder(
            "Sand-Sto Storage",
            "/qms onboarding/sand-sto storage",
        )];

        assert!(pick_facility_folder(folders, "Sand-Sto Climate Controlled Storage").is_none());
    }

    #[test]
    fn picks_nothing_when_multiple_candidates_have_no_exact_match() {
        let folders = vec![
            Entry::test_folder("Sand-Sto Storage", "/qms onboarding/sand-sto storage"),
            Entry::test_folder(
                "Sand-Sto Self Storage",
                "/qms onboarding/sand-sto self storage",
            ),
        ];

        assert!(pick_facility_folder(folders, "Sand-Sto Climate Controlled Storage").is_none());
    }

    #[test]
    fn picks_nothing_when_search_returns_no_candidates_at_all() {
        assert!(pick_facility_folder(vec![], "Nonexistent Facility").is_none());
    }

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

        let config = DropboxConfig::from_env()
            .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
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
            entries
                .iter()
                .any(|e| e.is_folder() && e.name == "Papa Ducks"),
            "expected to find the known 'Papa Ducks' subfolder"
        );
    }

    // Same real-network reasoning as the test above. Searches for a
    // known facility ("Highway 20 Self Storage", under client "Prairie
    // Enterprises LLC") by a facility-only term, verifying both that the
    // folder-only filter actually drops the many file-name matches this
    // query also hits (rent rolls, unit lists, templates) and that a
    // generic query term still surfaces the facility folder itself
    // despite not naming the client at all.
    #[tokio::test]
    #[ignore]
    async fn search_folders_finds_a_facility_by_name_alone() {
        let _ = dotenvy::from_filename(".env.local");

        let config = DropboxConfig::from_env()
            .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
        let client = DropboxClient::new(config);

        let folders = client
            .search_folders("Highway 20")
            .await
            .expect("search_folders should succeed against the real QMS Onboarding folder");

        assert!(
            folders.iter().all(Entry::is_folder),
            "every returned entry should be a folder, not a file match"
        );
        assert!(
            folders
                .iter()
                .any(|e| e.name == "Highway 20 Self Storage"
                    && e.path_display.contains("Prairie Enterprises LLC")),
            "expected to find the Highway 20 Self Storage facility folder without searching by client name"
        );
    }

    // Real-network, read-only: resolve_shared_link is the primary,
    // reliable path -- Highway 20's own real dropbox_folder_url, whose
    // name matches OO's facility name.
    #[tokio::test]
    #[ignore]
    async fn resolves_highway_20s_real_shared_link_to_its_actual_path() {
        let _ = dotenvy::from_filename(".env.local");

        let config = DropboxConfig::from_env()
            .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
        let client = DropboxClient::new(config);

        let found = client
            .resolve_shared_link(
                "https://www.dropbox.com/scl/fo/iptn5zwsl0c4tr74r4jfi/AH4kjB7xiOg16DHFgVJ7J1M?rlkey=ahi804fhw7d141w3gj2e45ltx&st=cinc446y&dl=0",
            )
            .await
            .expect("resolving a real facility's own shared link must succeed")
            .expect("Highway 20's own real link must resolve to a real path");

        assert!(found.is_folder());
        assert!(found.path_display.to_lowercase().contains("highway 20"));
    }

    // Real-network, read-only: the case that actually matters --
    // Sand-Sto's own real dropbox_folder_url resolves to its real
    // folder ("Sand-Sto Storage") even though OO's own facility name
    // ("Sand-Sto Climate Controlled Storage") doesn't match it at all.
    // Confirms this mechanism never depends on the name matching, unlike
    // find_facility_folder's own name-search fallback.
    #[tokio::test]
    #[ignore]
    async fn resolves_sand_stos_real_shared_link_despite_the_oo_name_mismatch() {
        let _ = dotenvy::from_filename(".env.local");

        let config = DropboxConfig::from_env()
            .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
        let client = DropboxClient::new(config);

        let found = client
            .resolve_shared_link(
                "https://www.dropbox.com/scl/fo/8yhn4pue198c2gzcqwyxt/AI6hvEdb_Mepxjumow8fAig?rlkey=5sn69x7ouu3kduvf9my112lww&st=bvd1b2gc&dl=0",
            )
            .await
            .expect("resolving a real facility's own shared link must succeed")
            .expect("Sand-Sto's own real link must resolve to a real path");

        assert!(found.is_folder());
        assert_eq!(found.name, "Sand-Sto Storage");
    }

    // Real-network, read-only: proves `find_facility_folder` locates the
    // real, writable path -- confirmed live 2026-09-04 to be the same
    // physical folder `dropbox_folder_url`'s own shared link points at
    // (same subfolders: Final Data, Preliminary Data, Tenants & Leases
    // Migration, Units Migration, Validation), reached by name search
    // under this app's own root as a fallback when `resolve_shared_link`
    // isn't available.
    #[tokio::test]
    #[ignore]
    async fn finds_a_real_facilitys_own_folder_by_exact_name() {
        let _ = dotenvy::from_filename(".env.local");

        let config = DropboxConfig::from_env()
            .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
        let client = DropboxClient::new(config);

        let found = client
            .find_facility_folder("Highway 20 Self Storage")
            .await
            .expect("searching for a real facility's folder must succeed")
            .expect("Highway 20 Self Storage's own folder must be found");

        assert!(found.is_folder());
        assert!(found
            .path_display
            .to_lowercase()
            .contains("prairie enterprises llc"));
    }

    // Real-network confirmation of the actual case found live 2026-09-04:
    // OO's own facility name ("Sand-Sto Climate Controlled Storage")
    // doesn't match its real Dropbox folder name ("Sand-Sto Storage") --
    // and searching for OO's own name doesn't even reliably surface the
    // real folder as a candidate at all (Dropbox's own search returned
    // exactly one result, and it was an unrelated folder,
    // `sand_sto_climate_control_storage_decrypt`). This must resolve to
    // nothing, not a wrong guess -- see `pick_facility_folder`'s own doc
    // comment for the full story of why a same-day fallback attempt here
    // was reverted.
    #[tokio::test]
    #[ignore]
    async fn resolves_to_nothing_for_a_facility_whose_dropbox_folder_name_differs_from_oos_name() {
        let _ = dotenvy::from_filename(".env.local");

        let config = DropboxConfig::from_env()
            .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
        let client = DropboxClient::new(config);

        let found = client
            .find_facility_folder("Sand-Sto Climate Controlled Storage")
            .await
            .expect("the search call itself must still succeed");

        assert!(
            found.is_none(),
            "must not guess at an unrelated folder when nothing matches exactly"
        );
    }

    // A name that matches files but no exact-named folder (every real
    // Highway 20 CSV export mentions the facility name) must not
    // false-positive on one of those files' own containing folder.
    #[tokio::test]
    #[ignore]
    async fn returns_none_when_no_folder_matches_the_name_exactly() {
        let _ = dotenvy::from_filename(".env.local");

        let config = DropboxConfig::from_env()
            .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
        let client = DropboxClient::new(config);

        let found = client
            .find_facility_folder("Highway 20 Self Storage Unit Coverages")
            .await
            .expect("the search itself must still succeed even with no exact match");

        assert!(found.is_none());
    }

    // Real-network, and genuinely mutating: creates a folder in the real
    // QMS Onboarding tree. Deliberately targets a path nested under the
    // real Highway 20 folder used by the test above (not a throwaway
    // top-level folder), named so it's unambiguous as a test artifact if
    // ever seen by a human. Run manually and clean up in Dropbox after --
    // not something to fire automatically.
    #[tokio::test]
    #[ignore]
    async fn create_folder_if_missing_is_idempotent_against_the_real_account() {
        let _ = dotenvy::from_filename(".env.local");

        let config = DropboxConfig::from_env()
            .expect("DROPBOX_* env vars must be set in .env.local to run this ignored test");
        let client = DropboxClient::new(config);
        let path = format!(
            "{}/_unitprep_dropbox_client_test_scratch",
            client.root_path()
        );

        client
            .create_folder_if_missing(&path)
            .await
            .expect("creating a genuinely new folder must succeed");
        client
            .create_folder_if_missing(&path)
            .await
            .expect("creating the same folder again must be treated as success, not an error");
    }
}
