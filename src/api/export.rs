//! Generates all export artifacts from the session's
//! cached AnalysisResults entirely in memory.
//!
//! Export flow:
//!
//! Session
//!   -> Validation Check
//!   -> Analysis Check
//!   -> Generate CSV/JSON artifacts
//!   -> Build ZIP in memory
//!   -> Return ZIP to browser
//!
//!   DESIGN RATIONALE
//! - Eliminates export-folder collisions
//! - Eliminates stale export artifacts
//! - Eliminates export cleanup requirements
//! - Reduces disk I/O
//! - Preserves session isolation
//!
//! No files are written to disk.

use std::time::Instant;

use axum::{
    extract::{Json, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::dropbox_browse::{ensure_path_in_root, parent_folder},
    api::{internal_error, session_not_found, stage_conflict, ApiErrorBody, AppState},
    application::unit_group_session::WorkflowStage,
    auth::AuthenticatedUser,
    client_ops::audit_log,
    infrastructure::csv_export,
};

#[derive(Debug, Deserialize)]
pub struct ExportRequest {
    pub session_id: String,
    /// The client this Unit Group run was for, when the session was
    /// opened from a client's own Unit Groups tab
    /// (`/clients/{clientId}/unit-groups`) -- `None` for a standalone
    /// run. Same reasoning as `dedup::DedupExportRequest::client_id`.
    #[serde(default)]
    pub client_id: Option<uuid::Uuid>,
}

/// What `generate_export_zip` produces -- the ZIP bytes plus everything
/// `export`/`export_to_dropbox` need for their own tracing/audit-log
/// calls, without either of them needing to re-derive it from the
/// `AnalysisResults` this function already read and then let go of.
struct GeneratedExport {
    zip_bytes: Vec<u8>,
    filename: String,
    file_count: usize,
    facilities_count: usize,
    net_new_groups_count: usize,
    similar_groups_count: usize,
}

/// The shared "validate, generate the ZIP, write back the Exported
/// stage" logic behind both `export` (returns the ZIP as a download)
/// and `export_to_dropbox` (uploads it instead) -- everything except
/// the final response/upload action and the audit-log call, since those
/// two differ (a `format`-less local download vs. a `dropbox_path`).
#[allow(clippy::result_large_err)]
async fn generate_export_zip(
    state: &AppState,
    user: &AuthenticatedUser,
    session_id: &str,
) -> Result<GeneratedExport, Response> {
    let started = Instant::now();

    //
    // Read-only session access.
    // This shape is deliberately future-proof for PR3.
    //
    let session_data = match state.unit_group_sessions.with_owned_session(
        session_id,
        user.user_id,
        |session| {
            if let Err(err) = session.require_stage(WorkflowStage::Analyzed) {
                tracing::warn!(
                    session_id = %session_id,
                    required = ?err.required,
                    current = ?err.current,
                    "Export attempted before validation/analysis completed"
                );

                return Err(err);
            }

            let validation = session
                .data
                .validation
                .clone()
                .expect("Analyzed stage guarantees validation data");

            let analysis = session
                .data
                .analysis
                .clone()
                .expect("Analyzed stage guarantees analysis data");

            Ok((validation, analysis, session.data_generation()))
        },
    ) {
        Some(Ok(data)) => data,
        Some(Err(err)) => {
            return Err(stage_conflict(session_id, err));
        }
        None => {
            return Err(session_not_found(session_id));
        }
    };

    let (validation, analysis, read_generation) = session_data;

    if !validation.ready {
        tracing::warn!(
            session_id = %session_id,
            issue_count = validation.issue_count,
            error_count = validation.error_count,
            "Export blocked by validation failures"
        );

        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "validation_unresolved",
                message: "Validation issues must be resolved before export".to_string(),
            }),
        )
            .into_response());
    }

    let has_exportable_content = !analysis.batch_run.facilities.is_empty()
        || !analysis.net_new_groups.is_empty()
        || !analysis.similar_groups.is_empty()
        || !analysis.batch_run.advisory_issues.is_empty();

    if !has_exportable_content {
        tracing::warn!(
            session_id = %session_id,
            "Export attempted with no exportable data"
        );

        return Err((
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "no_exportable_data",
                message: "No exportable data available".to_string(),
            }),
        )
            .into_response());
    }

    let export_files = match csv_export::generate_outputs(&analysis, true) {
        Ok(files) => files,
        Err(err) => {
            tracing::error!(
                session_id = %session_id,
                error = %err,
                "Failed generating export files"
            );

            return Err(internal_error("Failed generating export files"));
        }
    };

    let file_count = export_files.len();

    let zip_bytes = match csv_export::build_zip(export_files) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!(
                session_id = %session_id,
                error = %err,
                "Failed building export ZIP"
            );

            return Err(internal_error("Failed building export ZIP"));
        }
    };

    let timestamp = Utc::now().format("%Y-%m-%d_%H%M%S").to_string();
    let filename = format!("UnitPrep_Output_{}.zip", timestamp);

    //
    // Tiny mutation scope.
    //
    match state.unit_group_sessions.with_owned_session_mut(
        session_id,
        user.user_id,
        |session| {
            // Same TOCTOU concern as analyze.rs: a correction landing in
            // this gap already downgraded `workflow` back to `Validated`
            // as its own safety net — unconditionally calling
            // `complete_export` here would silently re-promote it to
            // `Exported` even though the ZIP just returned was built from
            // data that's no longer current.
            if session.data_generation() == read_generation {
                session.complete_export();
                true
            } else {
                false
            }
        },
    ) {
        Some(true) => {}

        Some(false) => {
            tracing::warn!(
                session_id = %session_id,
                "Session data changed during export — discarding the stale write-back so the workflow stage can't be falsely re-promoted to Exported"
            );
        }

        None => {
            // Same narrow race as analyze.rs: the session vanished
            // between the read lock above and this write-back. The ZIP
            // is already built and returned regardless — this just makes
            // the race observable rather than changing the response.
            tracing::warn!(
                session_id = %session_id,
                "Session no longer exists — export stage could not be recorded"
            );
        }
    }

    tracing::info!(
        session_id = %session_id,
        owner_id = %user.user_id,
        facilities = analysis.batch_run.facilities.len(),
        net_new_groups = analysis.net_new_groups.len(),
        similar_groups = analysis.similar_groups.len(),
        advisory_issues = analysis.batch_run.advisory_issues.len(),
        file_count = file_count,
        zip_size_bytes = zip_bytes.len(),
        export_ms = started.elapsed().as_millis(),
        zip_name = %filename,
        "Export generated successfully"
    );

    Ok(GeneratedExport {
        zip_bytes,
        filename,
        file_count,
        facilities_count: analysis.batch_run.facilities.len(),
        net_new_groups_count: analysis.net_new_groups.len(),
        similar_groups_count: analysis.similar_groups.len(),
    })
}

pub async fn export(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<ExportRequest>,
) -> Response {
    let generated = match generate_export_zip(&state, &user, &request.session_id).await {
        Ok(generated) => generated,
        Err(response) => return response,
    };

    audit_log::record(
        &state.db,
        audit_log::event::UNIT_GROUP_COMPLETED,
        user.user_id,
        "client",
        request.client_id.as_ref().map(ToString::to_string).as_deref(),
        audit_log::Change::none(),
        None,
        None,
        serde_json::json!({
            "session_id": request.session_id,
            "facilities": generated.facilities_count,
            "net_new_groups": generated.net_new_groups_count,
            "similar_groups": generated.similar_groups_count,
        }),
    )
    .await;

    let mut headers = HeaderMap::new();

    headers.insert(header::CONTENT_TYPE, "application/zip".parse().unwrap());

    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{}\"", generated.filename)
            .parse()
            .unwrap(),
    );

    (headers, generated.zip_bytes).into_response()
}

const GROUP_PREP_OUTPUT_FOLDER_NAME: &str = "Group Prep Output";

#[derive(Debug, Serialize)]
pub struct ExportSaveLocationResponse {
    /// Mirrors `dedup::DedupSaveLocationResponse`/`tagger::
    /// TaggerSaveLocationResponse` -- `Some(path)` when this session was
    /// imported from Dropbox (the one folder the user picked, not a
    /// per-file provenance model -- see `SessionData::
    /// source_dropbox_folder_path`'s own doc comment), `None` for a
    /// local upload.
    pub default_folder_path: Option<String>,
}

/// Mirrors `dedup::save_location`/`tagger::save_location`.
pub async fn save_location(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<ExportRequest>,
) -> Response {
    let source_folder = match state.unit_group_sessions.with_owned_session(
        &request.session_id,
        user.user_id,
        |session| session.data.source_dropbox_folder_path.clone(),
    ) {
        Some(source_folder) => source_folder,
        None => return session_not_found(&request.session_id),
    };

    let default_folder_path =
        source_folder.map(|folder| format!("{folder}/{GROUP_PREP_OUTPUT_FOLDER_NAME}"));

    Json(ExportSaveLocationResponse { default_folder_path }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct ExportToDropboxRequest {
    pub session_id: String,
    #[serde(default)]
    pub client_id: Option<uuid::Uuid>,
    /// Full destination path, filename included -- resolved by the
    /// frontend's one-click "Save to Facility Folder" default, not
    /// guessed at here.
    pub dropbox_path: String,
}

#[derive(Debug, Serialize)]
pub struct ExportToDropboxResponse {
    pub path: String,
}

/// Dropbox-destination counterpart to `export` -- same ZIP generation
/// via `generate_export_zip`, destination is a Dropbox path instead of
/// an HTTP response body. Mirrors `dedup::export_to_dropbox`.
pub async fn export_to_dropbox(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<ExportToDropboxRequest>,
) -> Response {
    if let Err(response) = ensure_path_in_root(&state, &request.dropbox_path) {
        return response;
    }

    let generated = match generate_export_zip(&state, &user, &request.session_id).await {
        Ok(generated) => generated,
        Err(response) => return response,
    };

    if let Some(folder) = parent_folder(&request.dropbox_path) {
        if let Err(err) = state.dropbox.create_folder_if_missing(&folder).await {
            tracing::error!(error = %err, path = %folder, "Dropbox create_folder_if_missing failed during unit-group export");
            return internal_error("Could not create the destination folder in Dropbox");
        }
    }

    if let Err(err) = state
        .dropbox
        .upload(&request.dropbox_path, generated.zip_bytes)
        .await
    {
        tracing::error!(error = %err, path = %request.dropbox_path, "Dropbox upload failed during unit-group export");
        return internal_error("Could not upload the export to Dropbox");
    }

    audit_log::record(
        &state.db,
        audit_log::event::UNIT_GROUP_COMPLETED,
        user.user_id,
        "client",
        request.client_id.as_ref().map(ToString::to_string).as_deref(),
        audit_log::Change::none(),
        None,
        None,
        serde_json::json!({
            "session_id": request.session_id,
            "facilities": generated.facilities_count,
            "net_new_groups": generated.net_new_groups_count,
            "similar_groups": generated.similar_groups_count,
            "dropbox_path": request.dropbox_path,
        }),
    )
    .await;

    tracing::info!(
        session_id = %request.session_id,
        owner_id = %user.user_id,
        path = %request.dropbox_path,
        file_count = generated.file_count,
        "Unit-group export saved to Dropbox"
    );

    Json(ExportToDropboxResponse { path: request.dropbox_path }).into_response()
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
