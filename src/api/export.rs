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
use serde::Deserialize;

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{internal_error, session_not_found, stage_conflict, ApiErrorBody, AppState},
    application::unit_group_session::WorkflowStage,
    auth::AuthenticatedUser,
    infrastructure::csv_export,
};

#[derive(Debug, Deserialize)]
pub struct ExportRequest {
    pub session_id: String,
}

pub async fn export(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<ExportRequest>,
) -> Response {
    let started = Instant::now();

    //
    // Read-only session access.
    // This shape is deliberately future-proof for PR3.
    //
    let session_data = match state.unit_group_sessions.with_owned_session(
        &request.session_id,
        user.user_id,
        |session| {
            if let Err(err) = session.require_stage(WorkflowStage::Analyzed) {
                tracing::warn!(
                    session_id = %request.session_id,
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
            return stage_conflict(&request.session_id, err);
        }
        None => {
            return session_not_found(&request.session_id);
        }
    };

    let (validation, analysis, read_generation) = session_data;

    if !validation.ready {
        tracing::warn!(
            session_id = %request.session_id,
            issue_count = validation.issue_count,
            error_count = validation.error_count,
            "Export blocked by validation failures"
        );

        return (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "validation_unresolved",
                message: "Validation issues must be resolved before export".to_string(),
            }),
        )
            .into_response();
    }

    let has_exportable_content = !analysis.batch_run.facilities.is_empty()
        || !analysis.net_new_groups.is_empty()
        || !analysis.similar_groups.is_empty()
        || !analysis.batch_run.advisory_issues.is_empty();

    if !has_exportable_content {
        tracing::warn!(
            session_id = %request.session_id,
            "Export attempted with no exportable data"
        );

        return (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "no_exportable_data",
                message: "No exportable data available".to_string(),
            }),
        )
            .into_response();
    }

    let export_files = match csv_export::generate_outputs(&analysis, true) {
        Ok(files) => files,
        Err(err) => {
            tracing::error!(
                session_id = %request.session_id,
                error = %err,
                "Failed generating export files"
            );

            return internal_error("Failed generating export files");
        }
    };

    let file_count = export_files.len();

    let zip_bytes = match csv_export::build_zip(export_files) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!(
                session_id = %request.session_id,
                error = %err,
                "Failed building export ZIP"
            );

            return internal_error("Failed building export ZIP");
        }
    };

    let timestamp = Utc::now().format("%Y-%m-%d_%H%M%S").to_string();

    let filename = format!("UnitPrep_Output_{}.zip", timestamp);

    //
    // Tiny mutation scope.
    //
    match state.unit_group_sessions.with_owned_session_mut(
        &request.session_id,
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
                session_id = %request.session_id,
                "Session data changed during export — discarding the stale write-back so the workflow stage can't be falsely re-promoted to Exported"
            );
        }

        None => {
            // Same narrow race as analyze.rs: the session vanished
            // between the read lock above and this write-back. The ZIP
            // is already built and returned regardless — this just makes
            // the race observable rather than changing the response.
            tracing::warn!(
                session_id = %request.session_id,
                "Session no longer exists — export stage could not be recorded"
            );
        }
    }

    tracing::info!(
        session_id = %request.session_id,
        owner_id = %user.user_id,
        facilities =
            analysis
                .batch_run
                .facilities
                .len(),
        net_new_groups =
            analysis
                .net_new_groups
                .len(),
        similar_groups =
            analysis
                .similar_groups
                .len(),
        advisory_issues =
            analysis
                .batch_run
                .advisory_issues
                .len(),
        file_count =
            file_count,
        zip_size_bytes =
            zip_bytes.len(),
        export_ms =
            started
                .elapsed()
                .as_millis(),
        zip_name =
            %filename,
        "Export generated successfully"
    );

    let mut headers = HeaderMap::new();

    headers.insert(header::CONTENT_TYPE, "application/zip".parse().unwrap());

    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{}\"", filename)
            .parse()
            .unwrap(),
    );

    (headers, zip_bytes).into_response()
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
