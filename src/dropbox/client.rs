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
        let access_token = self.access_token().await?;

        let link_response = self
            .http
            .post("https://api.dropboxapi.com/2/sharing/get_shared_link_metadata")
            .bearer_auth(&access_token)
            .json(&serde_json::json!({ "url": url }))
            .send()
            .await?;

        let status = link_response.status();
        let body = link_response.text().await?;

        if !status.is_success() {
            tracing::warn!(url = %url, status = status.as_u16(), body = %body, "Dropbox shared-link resolution failed, falling back to name search");
            return Ok(None);
        }

        let link_metadata: SharedLinkMetadataResponse = match serde_json::from_str(&body) {
            Ok(parsed) => parsed,
            Err(err) => {
                tracing::warn!(url = %url, error = %err, body = %body, "failed to parse shared-link metadata response, falling back to name search");
                return Ok(None);
            }
        };

        if link_metadata.tag != "folder" {
            tracing::warn!(url = %url, tag = %link_metadata.tag, "shared link does not point at a folder, falling back to name search");
            return Ok(None);
        }

        let metadata_response = self
            .http
            .post("https://api.dropboxapi.com/2/files/get_metadata")
            .bearer_auth(&access_token)
            .header("Dropbox-API-Path-Root", self.path_root_header())
            .json(&serde_json::json!({ "path": link_metadata.id }))
            .send()
            .await?;

        let status = metadata_response.status();
        let body = metadata_response.text().await?;

        if !status.is_success() {
            tracing::warn!(url = %url, id = %link_metadata.id, status = status.as_u16(), body = %body, "Dropbox get_metadata by shared-link id failed, falling back to name search");
            return Ok(None);
        }

        match serde_json::from_str::<Entry>(&body) {
            Ok(entry) => {
                tracing::info!(url = %url, path = %entry.path_display, "dropbox shared link resolved to a real path via its object id");
                Ok(Some(entry))
            }
            Err(err) => {
                tracing::warn!(url = %url, error = %err, body = %body, "failed to parse get_metadata response, falling back to name search");
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
    pub async fn find_facility_folder(&self, facility_name: &str) -> Result<Option<Entry>, DropboxError> {
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
            Entry::test_folder("Sand-Sto Self Storage", "/qms onboarding/sand-sto self storage"),
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

    // Real-network, read-only: resolve_shared_link is the primary,
    // reliable path -- Highway 20's own real dropbox_folder_url, whose
    // name matches OO's facility name.
    #[tokio::test]
    #[ignore]
    async fn resolves_highway_20s_real_shared_link_to_its_actual_path() {
        let _ = dotenvy::from_filename(".env.local");

        let config = DropboxConfig::from_env().expect(
            "DROPBOX_* env vars must be set in .env.local to run this ignored test",
        );
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

        let config = DropboxConfig::from_env().expect(
            "DROPBOX_* env vars must be set in .env.local to run this ignored test",
        );
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

        let config = DropboxConfig::from_env().expect(
            "DROPBOX_* env vars must be set in .env.local to run this ignored test",
        );
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
