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

use std::io::{Cursor, Write};
use std::time::Instant;

use axum::{
    extract::{Json, State},
    http::{
        header,
        HeaderMap,
        StatusCode,
    },
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::Deserialize;
use zip::{
    write::SimpleFileOptions,
    CompressionMethod,
    ZipWriter,
};

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{stage_conflict, AppState},
    domain::session::WorkflowStage,
    infrastructure::csv_export,
};

#[derive(Debug, Deserialize)]
pub struct ExportRequest {
    pub session_id: String,

    /// Explicit human override for exporting despite unresolved
    /// Severity::Error validation issues (e.g. after reviewing them via
    /// the inline correction UI and deciding to proceed anyway). Defaults
    /// to false so old clients that don't send this field keep the
    /// existing blocking behavior.
    #[serde(default)]
    pub acknowledge_errors: bool,
}

pub async fn export(
    State(state): State<AppState>,
    Json(request): Json<ExportRequest>,
) -> Response {
    let started = Instant::now();

    //
    // Read-only session access.
    // This shape is deliberately future-proof for PR3.
    //
    let session_data = match state
        .session_store
        .with_session(
            &request.session_id,
            |session| {
                if let Err(err) =
                    session.require_stage(
                        WorkflowStage::Analyzed,
                    )
                {
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
                    .expect(
                        "Analyzed stage guarantees validation data",
                    );

                let analysis = session
                    .data
                    .analysis
                    .clone()
                    .expect(
                        "Analyzed stage guarantees analysis data",
                    );

                Ok((
                    validation,
                    analysis,
                ))
            },
        ) {
        Some(Ok(data)) => data,
        Some(Err(err)) => {
            return stage_conflict(err);
        }
        None => {
            return (
                StatusCode::NOT_FOUND,
                "Session not found",
            )
                .into_response();
        }
    };

    let (validation, analysis) =
        session_data;

    if !validation.ready
        && !request.acknowledge_errors
    {
        tracing::warn!(
            session_id = %request.session_id,
            issue_count = validation.issue_count,
            error_count = validation.error_count,
            "Export blocked by validation failures"
        );

        return (
            StatusCode::BAD_REQUEST,
            "Validation issues must be resolved before export",
        )
            .into_response();
    }

    if !validation.ready
        && request.acknowledge_errors
    {
        tracing::warn!(
            session_id = %request.session_id,
            error_count = validation.error_count,
            "Export proceeding despite unresolved validation errors — acknowledged by user"
        );
    }

    let has_exportable_content =
        !analysis
            .batch_run
            .facilities
            .is_empty()
            || !analysis
                .net_new_groups
                .is_empty()
            || !analysis
                .similar_groups
                .is_empty()
            || !analysis
                .batch_run
                .advisory_issues
                .is_empty();

    if !has_exportable_content {
        tracing::warn!(
            session_id = %request.session_id,
            "Export attempted with no exportable data"
        );

        return (
            StatusCode::BAD_REQUEST,
            "No exportable data available",
        )
            .into_response();
    }

    let export_files =
        match csv_export::generate_outputs(
            &analysis,
            true,
        ) {
            Ok(files) => files,
            Err(err) => {
                tracing::error!(
                    session_id = %request.session_id,
                    error = %err,
                    "Failed generating export files"
                );

                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed generating export files",
                )
                    .into_response();
            }
        };

    let file_count =
        export_files.len();

    let mut cursor =
        Cursor::new(Vec::<u8>::new());

    {
        let mut zip =
            ZipWriter::new(
                &mut cursor,
            );

        let options =
            SimpleFileOptions::default()
                .compression_method(
                    CompressionMethod::Deflated,
                );

        for file in export_files {
            if let Err(err) =
                zip.start_file(
                    &file.file_name,
                    options,
                )
            {
                tracing::error!(
                    session_id = %request.session_id,
                    file_name = %file.file_name,
                    error = %err,
                    "Failed adding file to ZIP"
                );

                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed building ZIP",
                )
                    .into_response();
            }

            if let Err(err) =
                zip.write_all(
                    &file.bytes,
                )
            {
                tracing::error!(
                    session_id = %request.session_id,
                    file_name = %file.file_name,
                    error = %err,
                    "Failed writing ZIP entry"
                );

                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed building ZIP",
                )
                    .into_response();
            }
        }

        if let Err(err) =
            zip.finish()
        {
            tracing::error!(
                session_id = %request.session_id,
                error = %err,
                "Failed finalizing ZIP"
            );

            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed finalizing ZIP",
            )
                .into_response();
        }
    }

    let timestamp = Utc::now()
        .format("%Y-%m-%d_%H%M%S")
        .to_string();

    let filename = format!(
        "UnitPrep_Output_{}.zip",
        timestamp
    );

    let zip_bytes =
        cursor.into_inner();

    //
    // Tiny mutation scope.
    //
    let _ = state
        .session_store
        .with_session_mut(
            &request.session_id,
            |session| {
                session.complete_export();
            },
        );

    tracing::info!(
        session_id = %request.session_id,
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

    let mut headers =
        HeaderMap::new();

    headers.insert(
        header::CONTENT_TYPE,
        "application/zip"
            .parse()
            .unwrap(),
    );

    headers.insert(
        header::CONTENT_DISPOSITION,
        format!(
            "attachment; filename=\"{}\"",
            filename
        )
        .parse()
        .unwrap(),
    );

    (headers, zip_bytes)
        .into_response()
}


#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
