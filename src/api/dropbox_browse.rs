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
        Ok(entries) => {
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
}
