//! HTTP layer for the duplicate-tenant-check tool. Session-based, like
//! UnitGroup, but with only one real stage — see
//! `application::dedup_session_service` for why: no correction loop, no
//! in-app confirm/dismiss step, per the tool's MVP scope (list every
//! finding; corrections happen entirely outside the platform).

use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Json, Multipart, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use unitprep_core::session_store::SessionStoreExt;
use unitprep_core::uploaded_file::UploadedFile;
use unitprep_dedup::DedupReport;

use crate::api::{internal_error, session_not_found, ApiErrorBody, AppState};
use crate::application::dedup_session_service::DedupSessionService;
use crate::infrastructure::dedup_csv_export;

#[derive(Debug, Serialize)]
pub struct DedupCheckResponse {
    pub session_id: String,
    pub report: DedupReport,
}

#[derive(Debug, Deserialize)]
pub struct DedupSessionRequest {
    pub session_id: String,
}

/// Reads the first file field from `multipart` — a duplicate-tenant
/// check is always one QMS export file, not a multi-file upload like
/// UnitGroup's `/upload`. Extra fields beyond the first are logged and
/// ignored rather than treated as an error.
async fn first_uploaded_file(
    multipart: &mut Multipart,
) -> Result<Option<UploadedFile>, axum::extract::multipart::MultipartError> {
    let mut result = None;

    while let Some(field) = multipart.next_field().await? {
        let Some(file_name) = field.file_name().map(str::to_string) else {
            continue;
        };
        let relative_path = field.name().unwrap_or(&file_name).to_string();
        let bytes = field.bytes().await?.to_vec();

        if result.is_none() {
            result = Some(UploadedFile { file_name, relative_path, bytes });
        } else {
            tracing::warn!(
                file = %file_name,
                "Ignoring extra multipart field — duplicate-tenant check takes one file"
            );
        }
    }

    Ok(result)
}

/// Uploads and analyzes a QMS export file in one step, creating a new
/// dedup session. Combining upload+analyze (rather than UnitGroup's
/// separate stages) is deliberate: there's no ambiguity to resolve
/// in between, the check just runs.
pub async fn check(State(state): State<AppState>, mut multipart: Multipart) -> Response {
    let started = Instant::now();

    let file = match first_uploaded_file(&mut multipart).await {
        Ok(Some(file)) => file,
        Ok(None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiErrorBody {
                    error: "no_file_uploaded",
                    message: "No file was uploaded".to_string(),
                }),
            )
                .into_response();
        }
        Err(err) => {
            tracing::error!(error = %err, "Multipart parser error during dedup check");
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiErrorBody {
                    error: "multipart_error",
                    message: err.to_string(),
                }),
            )
                .into_response();
        }
    };

    let file_name = file.file_name.clone();

    let session_id = match DedupSessionService::new(Arc::clone(&state.dedup_sessions))
        .create_session(file)
    {
        Ok(id) => id,
        Err(err) => {
            // A parse/ingest failure here describes a problem with the
            // uploaded file itself (missing FirtLast column, unsupported
            // format, malformed CSV) — a data-quality issue safe to
            // surface directly, not an internal fault.
            tracing::warn!(file = %file_name, error = %err, "Dedup check failed to ingest file");
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiErrorBody { error: "invalid_file", message: err.to_string() }),
            )
                .into_response();
        }
    };

    let report = state
        .dedup_sessions
        .with_session(&session_id, |session| session.report.clone())
        .expect("session was just created and saved");

    tracing::info!(
        session_id = %session_id,
        file = %file_name,
        flagged_groups = report.flagged_groups.len(),
        typo_variant_candidates = report.typo_variant_candidates.len(),
        check_ms = started.elapsed().as_millis(),
        "Dedup check complete"
    );

    Json(DedupCheckResponse { session_id, report }).into_response()
}

/// Re-fetches a previously computed report — e.g. after a page refresh,
/// without re-uploading the file.
pub async fn report(
    State(state): State<AppState>,
    Json(request): Json<DedupSessionRequest>,
) -> Response {
    match state.dedup_sessions.with_session(&request.session_id, |session| session.report.clone())
    {
        Some(report) => Json(report).into_response(),
        None => session_not_found(),
    }
}

/// Exports the full report as CSV — flagged groups first, then any
/// typo/name-variant candidates. See `dedup_csv_export` for the shape.
pub async fn export(
    State(state): State<AppState>,
    Json(request): Json<DedupSessionRequest>,
) -> Response {
    let started = Instant::now();

    let session_data = match state
        .dedup_sessions
        .with_session(&request.session_id, |session| {
            (session.report.clone(), session.records.clone())
        }) {
        Some(data) => data,
        None => return session_not_found(),
    };

    let (report, records) = session_data;

    let csv_bytes = match dedup_csv_export::generate_csv(&report, &records) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!(
                session_id = %request.session_id,
                error = %err,
                "Failed generating dedup export CSV"
            );
            return internal_error("Failed generating export CSV");
        }
    };

    tracing::info!(
        session_id = %request.session_id,
        flagged_groups = report.flagged_groups.len(),
        typo_variant_candidates = report.typo_variant_candidates.len(),
        csv_size_bytes = csv_bytes.len(),
        export_ms = started.elapsed().as_millis(),
        "Dedup export generated"
    );

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "text/csv".parse().unwrap());
    headers.insert(
        header::CONTENT_DISPOSITION,
        "attachment; filename=\"duplicate_tenant_check.csv\"".parse().unwrap(),
    );

    (headers, csv_bytes).into_response()
}

#[cfg(test)]
#[path = "dedup_tests.rs"]
mod tests;
