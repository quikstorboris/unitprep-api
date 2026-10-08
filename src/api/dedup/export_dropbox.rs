//! `POST /dedup/export-dropbox` -- saves an export into the facility's Dropbox folder.

use super::dto::ExportFormat;
use super::export_bytes::compute_export_file_names;
use crate::api::dropbox_browse::ensure_path_in_root;
use crate::api::{dedup_blocking, internal_error, session_not_found, AppState};
use crate::auth::AuthenticatedUser;
use crate::client_ops::{audit_log, tool_runs};
use axum::extract::{Json, State};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use unitprep_core::session_store::SessionStoreExt;

#[derive(Debug, Deserialize)]
pub struct DedupExportToDropboxRequest {
    pub session_id: String,
    #[serde(default)]
    pub format: ExportFormat,
    /// Destination *folder* only -- resolved by the frontend's Dropbox
    /// folder picker. The filename is no longer the frontend's concern:
    /// the backend computes and appends it (see `dedup_filename`), the
    /// same way `export()`'s Content-Disposition filename always has
    /// been.
    pub folder_path: String,
    /// Same reasoning as `DedupExportRequest::client_id`.
    #[serde(default)]
    pub client_id: Option<uuid::Uuid>,
    /// Same reasoning as `DedupExportRequest::facility_id`.
    #[serde(default)]
    pub facility_id: Option<uuid::Uuid>,
}

#[derive(Debug, Serialize)]
pub struct DedupExportToDropboxResponse {
    pub path: String,
}

/// Dropbox-sourced counterpart to `export()` -- same report lookup and
/// byte generation via `generate_export`, destination is a Dropbox path
/// the frontend already resolved via its folder picker instead of an
/// HTTP response body.
pub async fn export_to_dropbox(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<DedupExportToDropboxRequest>,
) -> Response {
    let started = Instant::now();

    if let Err(response) = ensure_path_in_root(&state, &request.folder_path) {
        return response;
    }

    let session_data = match state.dedup_sessions.with_owned_session(
        &request.session_id,
        user.user_id,
        |session| (session.report.clone(), session.records.clone()),
    ) {
        Some(data) => data,
        None => return session_not_found(&request.session_id),
    };

    let (report, records) = session_data;

    // Dropbox-save always uses the timestamped standalone fallback (not
    // the plain static one `export()` keeps) when there's no facility --
    // see `dedup_filename::standalone_file_name`'s own doc comment for
    // why: `DropboxClient::upload` is overwrite-only, so two saves of
    // the same standalone session to the same folder would otherwise
    // silently clobber each other.
    let file_names = match compute_export_file_names(
        &state.db,
        &user,
        request.facility_id,
        &request.format,
        true,
    )
    .await
    {
        Ok(file_names) => file_names,
        Err(response) => return response,
    };

    let (bytes, content_type, _report, _records) = match dedup_blocking::export(
        request.format,
        request.session_id.clone(),
        report,
        records,
        (file_names.zip_csv.clone(), file_names.zip_xlsx.clone()),
    )
    .await
    {
        Ok(generated) => generated,
        Err(response) => return response,
    };

    let dropbox_path = format!(
        "{}/{}",
        request.folder_path.trim_end_matches('/'),
        file_names.outer
    );

    // Ensures the destination folder exists before writing to it --
    // covers the `Duplicate Check` subfolder specifically (never created
    // ahead of time, only computed as a suggested path by `save_location`
    // above), but is deliberately unconditional: any destination folder
    // this call is ever pointed at should exist by the time the upload
    // itself is attempted, not just the one this feature was built for.
    if let Err(err) = state
        .dropbox
        .create_folder_if_missing(&request.folder_path)
        .await
    {
        tracing::error!(error = %err, path = %request.folder_path, "Dropbox create_folder_if_missing failed during dedup export");
        return internal_error("Could not create the destination folder in Dropbox");
    }

    if let Err(err) = state.dropbox.upload(&dropbox_path, bytes.clone()).await {
        tracing::error!(error = %err, path = %dropbox_path, "Dropbox upload failed during dedup export");
        return internal_error("Could not upload export to Dropbox");
    }

    tool_runs::attach_output_dropbox(
        &state.db,
        user.user_id,
        &user.role_keys,
        &request.session_id,
        &dropbox_path,
        tool_runs::OutputFile {
            bytes,
            content_type,
            file_name: &file_names.outer,
        },
    )
    .await;

    // Make the share link now, in the background, and keep it with the run:
    // the ClickUp duplicate-check comment links this file, and asking
    // Dropbox for the link then would add a second to that update. Not
    // awaited -- the save is already done, and a missing link is simply
    // made when it is first needed.
    {
        let (dropbox, db) = (state.dropbox.clone(), state.db.clone());
        let (actor, roles) = (user.user_id, user.role_keys.clone());
        let (session_id, path) = (request.session_id.clone(), dropbox_path.clone());
        tokio::spawn(async move {
            match dropbox.shared_link(&path).await {
                Ok(link) => {
                    tool_runs::store_output_dropbox_link(
                        &db,
                        actor,
                        &roles,
                        &session_id,
                        &path,
                        &link,
                    )
                    .await
                }
                Err(err) => {
                    tracing::debug!(error = %err, path, "Dropbox share link not created at save time")
                }
            }
        });
    }

    audit_log::record(
        &state.db,
        audit_log::event::DEDUP_COMPLETED,
        user.user_id,
        "client",
        request
            .client_id
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        audit_log::Change::none(),
        None,
        None,
        serde_json::json!({
            "session_id": request.session_id,
            "format": format!("{:?}", request.format),
            "dropbox_path": dropbox_path,
        }),
    )
    .await;

    tracing::info!(
        session_id = %request.session_id,
        owner_id = %user.user_id,
        format = ?request.format,
        path = %dropbox_path,
        export_ms = started.elapsed().as_millis(),
        "Dedup export saved to Dropbox"
    );
    crate::api::slow_operation::warn_if_slow("dedup_export_dropbox", started.elapsed());

    Json(DedupExportToDropboxResponse { path: dropbox_path }).into_response()
}
