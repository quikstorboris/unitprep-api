//! Folder-level Dropbox calls: listing (with pagination), searching,
//! resolving a shared link to a path, finding a facility's folder and
//! creating a folder.

use super::dto::{
    DropboxError, Entry, ListFolderResponse, SearchV2Response, SharedLinkMetadataResponse,
    MAX_LIST_FOLDER_PAGES,
};
use super::DropboxClient;
use crate::integrations::http::{truncate_for_log, MAX_LOGGED_BODY_BYTES};

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
pub(super) fn pick_facility_folder(folders: Vec<Entry>, facility_name: &str) -> Option<Entry> {
    folders.into_iter().find(|f| f.name == facility_name)
}

impl DropboxClient {
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
        let reply = self.rpc(endpoint, &body, true).await?;

        if !reply.is_success() {
            tracing::error!(
                path = %path,
                endpoint,
                status = reply.status.as_u16(),
                body = %truncate_for_log(&reply.body, MAX_LOGGED_BODY_BYTES),
                "Dropbox list_folder failed"
            );
            return Err(reply.into_error());
        }

        reply.parse("list_folder response")
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
        let reply = self
            .rpc(
                "/2/files/search_v2",
                &serde_json::json!({
                    "query": query,
                    "options": {
                        "path": self.config.root_path,
                        "max_results": 100,
                        "filename_only": true,
                    },
                }),
                true,
            )
            .await?;

        if !reply.is_success() {
            tracing::error!(
                query = %query,
                status = reply.status.as_u16(),
                body = %truncate_for_log(&reply.body, MAX_LOGGED_BODY_BYTES),
                "Dropbox search failed"
            );
            return Err(reply.into_error());
        }

        let parsed: SearchV2Response = reply.parse("search response")?;

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
        let link = self
            .rpc(
                "/2/sharing/get_shared_link_metadata",
                &serde_json::json!({ "url": url }),
                false,
            )
            .await?;

        if !link.is_success() {
            tracing::warn!(url = %url, status = link.status.as_u16(), body = %truncate_for_log(&link.body, MAX_LOGGED_BODY_BYTES), "Dropbox shared-link resolution failed, falling back to name search");
            return Ok(None);
        }

        let link_metadata: SharedLinkMetadataResponse = match serde_json::from_str(&link.body) {
            Ok(parsed) => parsed,
            Err(err) => {
                tracing::warn!(url = %url, error = %err, body = %truncate_for_log(&link.body, MAX_LOGGED_BODY_BYTES), "failed to parse shared-link metadata response, falling back to name search");
                return Ok(None);
            }
        };

        if link_metadata.tag != "folder" {
            tracing::warn!(url = %url, tag = %link_metadata.tag, "shared link does not point at a folder, falling back to name search");
            return Ok(None);
        }

        let metadata = self
            .rpc(
                "/2/files/get_metadata",
                &serde_json::json!({ "path": link_metadata.id }),
                true,
            )
            .await?;

        if !metadata.is_success() {
            tracing::warn!(url = %url, id = %link_metadata.id, status = metadata.status.as_u16(), body = %truncate_for_log(&metadata.body, MAX_LOGGED_BODY_BYTES), "Dropbox get_metadata by shared-link id failed, falling back to name search");
            return Ok(None);
        }

        match serde_json::from_str::<Entry>(&metadata.body) {
            Ok(entry) => {
                tracing::info!(url = %url, path = %entry.path_display, "dropbox shared link resolved to a real path via its object id");
                Ok(Some(entry))
            }
            Err(err) => {
                tracing::warn!(url = %url, error = %err, body = %truncate_for_log(&metadata.body, MAX_LOGGED_BODY_BYTES), "failed to parse get_metadata response, falling back to name search");
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
        let reply = self
            .rpc(
                "/2/files/create_folder_v2",
                &serde_json::json!({ "path": path }),
                true,
            )
            .await?;

        if reply.is_success() {
            tracing::info!(path = %path, "dropbox create_folder_v2 succeeded");
            return Ok(());
        }

        // Dropbox reports "folder already exists" as a 409 carrying a
        // structured error tag in the body, not a distinct HTTP status.
        if reply.status.as_u16() == 409 && reply.body.contains("path/conflict/folder") {
            tracing::info!(path = %path, "dropbox create_folder_v2: folder already exists");
            return Ok(());
        }

        tracing::error!(
            path = %path,
            status = reply.status.as_u16(),
            body = %truncate_for_log(&reply.body, MAX_LOGGED_BODY_BYTES),
            "Dropbox create_folder_v2 failed"
        );
        Err(reply.into_error())
    }
}
