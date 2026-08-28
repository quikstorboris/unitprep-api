//! Read-only Dropbox folder browsing for the client-setup flow (picking
//! `Client.dropboxPath` in `unitprep-ui`) -- lists folder names only,
//! nothing about file contents.
//!
//! The one thing standing between this endpoint and the entire company
//! Dropbox is the root check below: `state.dropbox`'s token has Full
//! Dropbox access (see `src/dropbox`'s module doc for why), and Dropbox
//! enforces no folder boundary on it at all. Every request here must stay
//! under `state.dropbox.root_path()`.

use axum::{
    extract::{Json, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::AuthenticatedUser;

#[derive(Debug, Deserialize)]
pub struct ListFolderQuery {
    /// Omitted (or empty) means "the configured root" -- what the
    /// frontend's folder picker asks for on first load.
    pub path: Option<String>,
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

    if path != root && !path.starts_with(&format!("{root}/")) {
        return path_outside_root(&path);
    }

    match state.dropbox.list_folder(&path).await {
        Ok(mut entries) => {
            // Folder-picker use case only (see this module's doc comment)
            // -- files living alongside client folders (e.g. QMS's own
            // "Default Permissions - ..." reference images at various
            // levels of the tree) have no business appearing in a picker
            // meant to select a folder. A future "browse everything"
            // view (e.g. a client's DrBx tab) is a different endpoint's
            // job, not a flag on this one.
            entries.retain(|entry| entry.is_folder());

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
