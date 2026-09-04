//! Read-only Dropbox folder browsing for the client-setup flow (picking
//! `Client.dropboxPath` in `unitprep-ui`) and for the Dedup Dropbox
//! import/export pickers -- lists folder names by default; pass
//! `include_files=true` to also see files, which the folder-only client
//! picker never asks for.
//!
//! The one thing standing between this endpoint and the entire company
//! Dropbox is the root check below (`ensure_path_in_root`): `state.dropbox`'s
//! token has Full Dropbox access (see `src/dropbox`'s module doc for why),
//! and Dropbox enforces no folder boundary on it at all. Every
//! caller-supplied path anywhere in this app must be checked against it
//! before being handed to `state.dropbox`.

use axum::{
    extract::{Json, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};

#[derive(Debug, Deserialize)]
pub struct ListFolderQuery {
    /// Omitted (or empty) means "the configured root" -- what the
    /// frontend's folder picker asks for on first load.
    pub path: Option<String>,

    /// Omitted (or false) filters the listing to folders only -- the
    /// client-setup picker's need. `true` is for a file-picker use case
    /// (e.g. Dedup's "Import from Dropbox"), which needs to see and
    /// select files, not just navigate through them.
    pub include_files: Option<bool>,
}

/// Deliberately just name/path/is_folder -- enough for a picker, not the
/// full Dropbox metadata shape. See `dropbox::Entry`, which this is built
/// from rather than serializing directly (keeping the wire-format-facing
/// type and the API-response-facing type independently free to change).
#[derive(Debug, Serialize)]
pub struct FolderEntry {
    pub name: String,
    pub path_display: String,
    pub is_folder: bool,
}

#[derive(Debug, Serialize)]
pub struct ListFolderResponse {
    pub path: String,
    pub entries: Vec<FolderEntry>,
}

fn path_outside_root(path: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "path_outside_dropbox_root",
            message: format!("{path} is outside the configured Dropbox root"),
        }),
    )
        .into_response()
}

/// Shared boundary check for every Dropbox-touching endpoint in the app,
/// not just this module's own `list_folder`/`search_folders` -- Dedup's
/// import/export handlers (`api::dedup`) call this too before handing a
/// caller-supplied path to `state.dropbox`. See this module's doc comment
/// for why the check has to live somewhere and be actually applied
/// everywhere.
#[allow(clippy::result_large_err)]
pub fn ensure_path_in_root(state: &AppState, path: &str) -> Result<(), Response> {
    let root = state.dropbox.root_path();

    if path == root || path.starts_with(&format!("{root}/")) {
        Ok(())
    } else {
        Err(path_outside_root(path))
    }
}

/// Any authenticated caller -- folder *names* only, nothing sensitive,
/// same reasoning as `client_ops_qms_tags::list_qms_tags`. No DB
/// transaction: this never touches Postgres, only proxies to Dropbox.
pub async fn list_folder(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Query(query): Query<ListFolderQuery>,
) -> Response {
    let root = state.dropbox.root_path();
    let path = query
        .path
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| root.to_string());

    if let Err(response) = ensure_path_in_root(&state, &path) {
        return response;
    }

    match state.dropbox.list_folder(&path).await {
        Ok(mut entries) => {
            // Folder-only is the default (see `ListFolderQuery::
            // include_files`'s doc comment) -- files living alongside
            // client folders (e.g. QMS's own "Default Permissions - ..."
            // reference images at various levels of the tree) have no
            // business appearing in a picker meant to select a folder.
            if !query.include_files.unwrap_or(false) {
                entries.retain(|entry| entry.is_folder());
            }

            // Dropbox does not guarantee alphabetical order -- sort here
            // rather than leaving the frontend to do it, so every
            // consumer of this endpoint gets a consistently ordered list
            // for free.
            entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

            // User info belongs here, not inside DropboxClient itself:
            // this handler knows *who* asked, the client below only
            // knows *what* was asked -- see dropbox::client's own
            // logging for the technical (user-agnostic) side of this
            // same call.
            tracing::info!(
                user_id = %user.user_id,
                path = %path,
                entry_count = entries.len(),
                "user browsed a Dropbox folder"
            );

            Json(ListFolderResponse {
                path,
                entries: entries
                    .into_iter()
                    .map(|entry| FolderEntry {
                        is_folder: entry.is_folder(),
                        name: entry.name,
                        path_display: entry.path_display,
                    })
                    .collect(),
            })
            .into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, path = %path, "dropbox list_folder failed");
            internal_error("Could not list Dropbox folder")
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SearchFoldersQuery {
    pub q: String,
}

#[derive(Debug, Serialize)]
pub struct SearchFoldersResponse {
    pub entries: Vec<FolderEntry>,
}

/// Any authenticated caller, same reasoning as `list_folder` above. No
/// root-boundary check needed here (unlike `list_folder`): the search
/// path passed to Dropbox is always `state.dropbox`'s own configured
/// root, never anything caller-supplied, so there is no path for a
/// request to escape it in the first place.
pub async fn search_folders(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Query(query): Query<SearchFoldersQuery>,
) -> Response {
    let q = query.q.trim();

    // A one-character query gets crowded out by file-name noise before
    // it says anything useful (see search_folders's own doc comment on
    // why over-fetching still has a ceiling) -- same guard most search
    // boxes apply, just enforced server-side too.
    if q.chars().count() < 2 {
        return Json(SearchFoldersResponse { entries: vec![] }).into_response();
    }

    match state.dropbox.search_folders(q).await {
        Ok(mut entries) => {
            entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

            tracing::info!(
                user_id = %user.user_id,
                query = %q,
                result_count = entries.len(),
                "user searched Dropbox folders"
            );

            Json(SearchFoldersResponse {
                entries: entries
                    .into_iter()
                    .map(|entry| FolderEntry {
                        is_folder: entry.is_folder(),
                        name: entry.name,
                        path_display: entry.path_display,
                    })
                    .collect(),
            })
            .into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, query = %q, "dropbox search_folders failed");
            internal_error("Could not search Dropbox")
        }
    }
}

fn not_found(entity: &'static str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ApiErrorBody {
            error: "not_found",
            message: format!("{entity} not found"),
        }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct FacilityDropboxFolderQuery {
    /// A facility name already known to the caller (e.g. `Client.
    /// facilityNames` in `unitprep-ui`, which carries names only, no
    /// ids -- see `api::clients_companies::CompanySummary`) -- taken by
    /// name rather than requiring a facility id specifically *because*
    /// the underlying lookup (`DropboxClient::find_facility_folder`) is
    /// itself name-based; requiring an id would only mean looking its
    /// name back up here for no real benefit.
    pub facility_name: String,
}

#[derive(Debug, Serialize)]
pub struct FacilityDropboxFolderResponse {
    /// `null` when this facility has no folder findable by exact name
    /// under the connected Dropbox root -- not an error, just "nothing to
    /// default to" (a brand-new facility whose folder hasn't been created
    /// yet, or one named differently there than in OO). The frontend
    /// falls back to today's behavior (no seeded `initialPath`) in that
    /// case.
    pub path: Option<String>,
}

/// A facility's own Dropbox folder -- tries its own captured
/// `dropbox_folder_url` first (`DropboxClient::resolve_shared_link`,
/// the reliable path since it's the real link PS captured, not a name
/// guess), falling back to an exact-name search under this app's own
/// root (`DropboxClient::find_facility_folder`) when there's no link at
/// all or it fails to resolve. See both of those methods' own doc
/// comments for the full reasoning.
///
/// Any authenticated caller -- same reasoning as `list_folder`/
/// `search_folders` above (read-only discovery, nothing sensitive). The
/// `company_id`/name check below exists so this can't be used to probe
/// for facility names outside the caller's own knowledge, not as a
/// permission gate on the Dropbox call itself.
pub async fn facility_dropbox_folder(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
    Query(query): Query<FacilityDropboxFolderQuery>,
) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for facility dropbox folder lookup");
            return internal_error("Could not look up this facility's Dropbox folder");
        }
    };

    let facility: Option<(Option<String>,)> = match sqlx::query_as(
        "SELECT dropbox_folder_url FROM clients.facilities WHERE company_id = $1 AND name = $2",
    )
    .bind(company_id)
    .bind(&query.facility_name)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for dropbox folder failed");
            return internal_error("Could not look up this facility's Dropbox folder");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit facility dropbox folder transaction");
        return internal_error("Could not look up this facility's Dropbox folder");
    }

    let Some((dropbox_folder_url,)) = facility else {
        return not_found("facility");
    };
    let name = query.facility_name;

    if let Some(url) = &dropbox_folder_url {
        match state.dropbox.resolve_shared_link(url).await {
            Ok(Some(entry)) => {
                tracing::info!(
                    user_id = %user.user_id,
                    company_id = %company_id,
                    facility_name = %name,
                    via = "shared_link",
                    "resolved a facility's default Dropbox folder"
                );
                return Json(FacilityDropboxFolderResponse { path: Some(entry.path_display) }).into_response();
            }
            Ok(None) => {} // falls through to the name-search fallback below
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, facility_name = %name, "dropbox resolve_shared_link failed");
                return internal_error("Could not look up this facility's Dropbox folder");
            }
        }
    }

    match state.dropbox.find_facility_folder(&name).await {
        Ok(found) => {
            tracing::info!(
                user_id = %user.user_id,
                company_id = %company_id,
                facility_name = %name,
                via = "name_search",
                found = found.is_some(),
                "resolved a facility's default Dropbox folder"
            );
            Json(FacilityDropboxFolderResponse {
                path: found.map(|entry| entry.path_display),
            })
            .into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, facility_name = %name, "dropbox find_facility_folder failed");
            internal_error("Could not look up this facility's Dropbox folder")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    // Deliberately the only test here: a path within the configured root
    // would reach the real network via `state.dropbox.list_folder` (see
    // `dropbox::client`'s own `#[ignore]`d real-credential test for that
    // coverage) -- this one exercises the one piece of logic that's
    // actually this handler's own, and does it without any network call
    // at all, since the rejection happens before `list_folder` is ever
    // reached.
    #[tokio::test]
    async fn rejects_a_path_outside_the_configured_root_without_calling_dropbox() {
        let response = list_folder(
            State(empty_state()),
            test_user(),
            Query(ListFolderQuery {
                path: Some("/Not/Under/The/Configured/Root".to_string()),
                include_files: None,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // Same reasoning as the test above: a query under the minimum length
    // returns before ever reaching state.dropbox.search_folders, so this
    // is real coverage of this handler's own logic without a network call.
    #[tokio::test]
    async fn rejects_a_one_character_query_without_calling_dropbox() {
        let response = search_folders(
            State(empty_state()),
            test_user(),
            Query(SearchFoldersQuery { q: "a".to_string() }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();

        assert_eq!(body.as_ref(), br#"{"entries":[]}"#);
    }
}
