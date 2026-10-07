//! Saving a tagged template into the facility's Dropbox folder, and the download response.

use super::apply::{
    build_edited_docx, record_applied, ConfirmedSubstitution, TaggerApplyRequest, DOCX_CONTENT_TYPE,
};
use super::views::TaggerSessionRequest;
use crate::api::dropbox_browse::{ensure_path_in_root, parent_folder};
use crate::api::{internal_error, session_not_found, AppState};
use crate::auth::AuthenticatedUser;
use crate::client_ops::tool_runs;
use axum::extract::{Json, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use unitprep_core::session_store::SessionStoreExt;

pub(super) const TAGGED_TEMPLATES_FOLDER_NAME: &str = "Tagged Templates";

#[derive(Debug, Serialize)]
pub struct TaggerSaveLocationResponse {
    /// `Some(path)` when this session's source file was imported from
    /// Dropbox -- the `Tagged Templates` subfolder next to wherever that
    /// file actually came from, which the frontend's save-to-Dropbox
    /// picker should default `initialPath` to. `None` for a
    /// locally-uploaded session. Mirrors `dedup::save_location`'s own
    /// response shape exactly.
    pub default_folder_path: Option<String>,
}

/// Mirrors `dedup::save_location` -- computes (but does not create)
/// this session's default save-to-Dropbox location.
pub async fn save_location(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<TaggerSessionRequest>,
) -> Response {
    let source_folder = match state.tagger_sessions.with_owned_session(
        &request.session_id,
        user.user_id,
        |session| session.source_dropbox_folder_path.clone(),
    ) {
        Some(source_folder) => source_folder,
        None => return session_not_found(&request.session_id),
    };

    let default_folder_path =
        source_folder.map(|folder| format!("{folder}/{TAGGED_TEMPLATES_FOLDER_NAME}"));

    Json(TaggerSaveLocationResponse {
        default_folder_path,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct TaggerApplyToDropboxRequest {
    pub session_id: String,
    pub confirmed: Vec<ConfirmedSubstitution>,
    #[serde(default)]
    pub preserve_blanks: bool,
    /// Full destination path, filename included -- resolved by the
    /// frontend's Dropbox folder picker (or the one-click "Save to
    /// Facility Folder" default), not guessed at here.
    pub dropbox_path: String,
}

#[derive(Debug, Serialize)]
pub struct TaggerApplyToDropboxResponse {
    pub path: String,
}

/// Dropbox-destination counterpart to `apply` -- same edit-building via
/// `build_edited_docx`, destination is a Dropbox path instead of an HTTP
/// response body. Mirrors `dedup::export_to_dropbox`.
pub async fn apply_to_dropbox(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<TaggerApplyToDropboxRequest>,
) -> Response {
    if let Err(response) = ensure_path_in_root(&state, &request.dropbox_path) {
        return response;
    }

    let apply_request = TaggerApplyRequest {
        session_id: request.session_id.clone(),
        confirmed: request.confirmed,
        preserve_blanks: request.preserve_blanks,
    };
    let (edited_bytes, file_name) = match build_edited_docx(&state, &user, &apply_request).await {
        Ok(built) => built,
        Err(response) => return response,
    };

    if let Some(folder) = parent_folder(&request.dropbox_path) {
        if let Err(err) = state.dropbox.create_folder_if_missing(&folder).await {
            tracing::error!(error = %err, path = %folder, "Dropbox create_folder_if_missing failed during tagger apply");
            return internal_error("Could not create the destination folder in Dropbox");
        }
    }

    if let Err(err) = state
        .dropbox
        .upload(&request.dropbox_path, edited_bytes.clone())
        .await
    {
        tracing::error!(error = %err, path = %request.dropbox_path, "Dropbox upload failed during tagger apply");
        return internal_error("Could not upload the tagged document to Dropbox");
    }

    record_applied(
        &state,
        &user,
        &request.session_id,
        apply_request.confirmed.len(),
        apply_request.preserve_blanks,
    )
    .await;
    tool_runs::attach_output_dropbox(
        &state.db,
        user.user_id,
        &user.role_keys,
        &request.session_id,
        &request.dropbox_path,
        tool_runs::OutputFile {
            bytes: edited_bytes,
            content_type: DOCX_CONTENT_TYPE,
            file_name: &file_name,
        },
    )
    .await;

    tracing::info!(
        session_id = %request.session_id,
        owner_id = %user.user_id,
        path = %request.dropbox_path,
        "Tagger apply saved to Dropbox"
    );

    Json(TaggerApplyToDropboxResponse {
        path: request.dropbox_path,
    })
    .into_response()
}

pub(super) fn tagged_file_name(original: &str) -> String {
    match original.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}-tagged.{ext}"),
        None => format!("{original}-tagged"),
    }
}

pub(super) fn file_response(bytes: Vec<u8>, file_name: &str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            .parse()
            .unwrap(),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{file_name}\"")
            .parse()
            .unwrap(),
    );
    (headers, bytes).into_response()
}
