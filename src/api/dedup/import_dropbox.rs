//! `POST /dedup/import-dropbox` -- the same check, reading the files from a Dropbox folder.

use super::dto::DedupCheckResponse;
use super::session::create_dedup_session;
use super::upload::guess_content_type;
use crate::api::dropbox_browse::{download_as_uploaded_file, ensure_path_in_root, parent_folder};
use crate::api::{dedup_blocking, ApiErrorBody, AppState};
use crate::auth::AuthenticatedUser;
use crate::client_ops::tool_runs;
use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::time::Instant;

/// Creates a session and a `tool_runs` row, so `facility_id` is required
/// here. Same reasoning as `DedupCheckQuery::facility_id` -- required, no
/// `#[serde(default)]`.
#[derive(Debug, Deserialize)]
pub struct DedupImportDropboxRequest {
    /// The files to check. A caller that still sends the original single
    /// `path` is treated as a one-element `paths`.
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub path: Option<String>,
    pub facility_id: uuid::Uuid,
}

impl DedupImportDropboxRequest {
    pub(super) fn selected_paths(&self) -> Vec<String> {
        let mut paths = self.paths.clone();
        if let Some(path) = &self.path {
            if !paths.contains(path) {
                paths.push(path.clone());
            }
        }
        paths
    }
}

/// Dropbox-sourced counterpart to `check()` -- same ingest/session-create
/// logic via `download_as_uploaded_file`, called only after the
/// frontend's confirm-vendor checkbox, exactly like `handleCheck` calls
/// `/dedup/check` today after a local upload.
pub async fn import_from_dropbox(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<DedupImportDropboxRequest>,
) -> Response {
    let started = Instant::now();

    let paths = request.selected_paths();
    if paths.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "no_file_uploaded",
                message: "No file was selected".to_string(),
            }),
        )
            .into_response();
    }

    for path in &paths {
        if let Err(response) = ensure_path_in_root(&state, path) {
            return response;
        }
    }

    // Downloads run a few at a time instead of one after another;
    // `buffered` keeps the results in selection order. The first failure
    // (in that order) is what the caller sees, as before.
    let state_ref = &state;
    let downloads = crate::integrations::http::join_all_bounded(
        paths
            .iter()
            .map(|path| async move { download_as_uploaded_file(state_ref, path).await }),
    )
    .await;

    let mut files = Vec::with_capacity(paths.len());
    for result in downloads {
        match result {
            Ok(file) => files.push(file),
            Err(response) => return response,
        }
    }

    // The folder the files came from, for the save-location default: the
    // first file's parent (a run's files all come from one folder).
    let source_dropbox_folder_path = parent_folder(&paths[0]);

    // Synchronous reads of the in-memory registry snapshots -- see
    // `client_ops::vendor_format`'s module doc comment for why these are
    // never per-request DB calls.
    let tenant_vendors = state.tenant_vendors.read().clone();
    let file_meta = state.tenant_file_meta.read().clone();

    let (files, created) = match create_dedup_session(
        &state,
        files,
        Some(user.user_id),
        tenant_vendors,
        file_meta,
        source_dropbox_folder_path,
    )
    .await
    {
        Ok(result) => result,
        Err(response) => return response,
    };

    let (session_id, report, records, ingested) = match created {
        Ok(created) => created,
        Err(err) => {
            tracing::warn!(files = files.len(), error = %err, "Dedup import-from-dropbox failed to ingest the selected files");
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiErrorBody {
                    error: "invalid_file",
                    message: err.to_string(),
                }),
            )
                .into_response();
        }
    };

    let file_name = files[ingested].file_name.clone();
    let source_bytes = files[ingested].bytes.clone();
    let ingested_path = paths[ingested].clone();

    tracing::info!(
        session_id = %session_id,
        owner_id = %user.user_id,
        path = %ingested_path,
        flagged_groups = report.flagged_groups.len(),
        typo_variant_candidates = report.typo_variant_candidates.len(),
        check_ms = started.elapsed().as_millis(),
        "Dedup check complete (imported from Dropbox)"
    );
    crate::api::slow_operation::warn_if_slow("dedup_check_dropbox", started.elapsed());

    // The view assembles the whole export plan: CPU-bound, so off the
    // async workers. `records` comes back for the tool-run record below.
    let (report, _, records) = match dedup_blocking::report_view(report, records).await {
        Ok(built) => built,
        Err(response) => return response,
    };

    tool_runs::create_dedup_run(
        &state.db,
        tool_runs::ToolRunCreate {
            facility_id: request.facility_id,
            session_id: &session_id,
            actor_user_id: user.user_id,
            role_keys: &user.role_keys,
            source_file_name: ingested_path.rsplit('/').next().unwrap_or(&ingested_path),
            source_dropbox_path: Some(&ingested_path),
            source_bytes,
            source_content_type: guess_content_type(&file_name),
            report_summary: serde_json::to_value(&report).unwrap_or_default(),
            records,
        },
    )
    .await;

    Json(DedupCheckResponse { session_id, report }).into_response()
}
