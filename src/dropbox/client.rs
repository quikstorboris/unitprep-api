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
        let access_token = self.access_token().await?;

        let response = self
            .http
            .post("https://api.dropboxapi.com/2/files/search_v2")
            .bearer_auth(access_token)
            .header("Dropbox-API-Path-Root", self.path_root_header())
            .json(&serde_json::json!({
                "query": query,
                "options": {
                    "path": self.config.root_path,
                    "max_results": 100,
                    "filename_only": true,
                },
            }))
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            tracing::error!(
                query = %query,
                status = status.as_u16(),
                body = %body,
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

    /// Finds a facility's own Dropbox folder by name under this app's
    /// connected root, and returns its real, writable path.
    ///
    /// **Not** resolved from `clients.facilities.dropbox_folder_url`
    /// itself -- confirmed live against the real API (2026-09-04) that
    /// these `https://www.dropbox.com/scl/fo/...` links are shared by an
    /// individual staff member's own Dropbox account (PS's
    /// `Facility_Onboarding_folder_URL:` field is filled in by hand),
    /// which is not necessarily the same Dropbox Business team this app's
    /// own token belongs to. `sharing/get_shared_link_metadata` on such a
    /// link comes back with real name/type metadata but no `path_lower`
    /// (no filesystem-level view) and only `link_access_level: viewer` --
    /// enough to prove the link resolves to *something*, not enough to
    /// list, create a folder in, or upload to it via this account.
    ///
    /// The same physical facility folder is reachable anyway: it lives
    /// under this app's own connected root (the same "QMS Onboarding"
    /// tree `search_folders` already searches for the client-search
    /// page's facility lookup), fully writable there. So this reuses
    /// `search_folders` -- proven working in production already -- rather
    /// than the URL at all, and returns the first folder result whose
    /// name matches exactly (a facility name search can otherwise surface
    /// files that merely mention it, e.g. "Highway 20 Self Storage Unit
    /// Coverages.csv", which `search_folders`'s own folder-only filter
    /// already drops, but a same-named sub-item one level down could
    /// still slip through a substring match -- exact-name equality is
    /// the one signal that reliably means "this IS the facility folder,"
    /// not just A a folder somewhere under it).
    pub async fn find_facility_folder(&self, facility_name: &str) -> Result<Option<Entry>, DropboxError> {
        let folders = self.search_folders(facility_name).await?;
        Ok(folders.into_iter().find(|f| f.name == facility_name))
    }

    /// Creates `path` as a folder if it doesn't already exist -- the
    /// `Duplicate Check` subfolder this app auto-creates next to wherever
    /// a source file was imported from is the one real caller. A
    /// `path/conflict/folder` error (the folder is already there) is
    /// treated as success, since the caller's actual goal -- "this folder
    /// exists" -- is already satisfied; every other error propagates.
    pub async fn create_folder_if_missing(&self, path: &str) -> Result<(), DropboxError> {
        let access_token = self.access_token().await?;

        let response = self
            .http
            .post("https://api.dropboxapi.com/2/files/create_folder_v2")
            .bearer_auth(access_token)
            .header("Dropbox-API-Path-Root", self.path_root_header())
            .json(&serde_json::json!({ "path": path }))
            .send()
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
            body = %body,
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
    /// only policy the one caller (api::dedup's export_to_dropbox) needs;
    /// the frontend guards against silent clobbering itself by always
    /// generating a timestamped filename, not by asking Dropbox to
    /// detect a conflict.
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

        let config = DropboxConfig::from_env().expect(
            "DROPBOX_* env vars must be set in .env.local to run this ignored test",
        );
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

    // Real-network, read-only: proves `find_facility_folder` locates the
    // real, writable path -- confirmed live 2026-09-04 to be the same
    // physical folder `dropbox_folder_url`'s own shared link points at
    // (same subfolders: Final Data, Preliminary Data, Tenants & Leases
    // Migration, Units Migration, Validation), reached by name search
    // under this app's own root instead, since the URL itself resolves
    // to no usable path (see this method's own doc comment).
    #[tokio::test]
    #[ignore]
    async fn finds_a_real_facilitys_own_folder_by_exact_name() {
        let _ = dotenvy::from_filename(".env.local");

        let config = DropboxConfig::from_env().expect(
            "DROPBOX_* env vars must be set in .env.local to run this ignored test",
        );
        let client = DropboxClient::new(config);

        let found = client
            .find_facility_folder("Highway 20 Self Storage")
            .await
            .expect("searching for a real facility's folder must succeed")
            .expect("Highway 20 Self Storage's own folder must be found");

        assert!(found.is_folder());
        assert!(found.path_display.to_lowercase().contains("prairie enterprises llc"));
    }

    // A name that matches files but no exact-named folder (every real
    // Highway 20 CSV export mentions the facility name) must not
    // false-positive on one of those files' own containing folder.
    #[tokio::test]
    #[ignore]
    async fn returns_none_when_no_folder_matches_the_name_exactly() {
        let _ = dotenvy::from_filename(".env.local");

        let config = DropboxConfig::from_env().expect(
            "DROPBOX_* env vars must be set in .env.local to run this ignored test",
        );
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

        let config = DropboxConfig::from_env().expect(
            "DROPBOX_* env vars must be set in .env.local to run this ignored test",
        );
        let client = DropboxClient::new(config);
        let path = format!("{}/_unitprep_dropbox_client_test_scratch", client.root_path());

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
