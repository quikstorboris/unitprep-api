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
use unitprep_dedup::{DedupReport, TenantRecord};

use crate::api::{internal_error, session_not_found, ApiErrorBody, AppState};
use crate::application::dedup_session_service::DedupSessionService;
use crate::infrastructure::csv_export::{build_zip, ExportFile};
use crate::infrastructure::{dedup_csv_export, dedup_xlsx_export};

#[derive(Debug, Serialize)]
pub struct DedupCheckResponse {
    pub session_id: String,
    pub report: DedupReport,
}

#[derive(Debug, Deserialize)]
pub struct DedupSessionRequest {
    pub session_id: String,
}

/// Which file format(s) `/dedup/export` should return. Defaults to
/// `Csv` via `#[serde(default)]` on the field below, so an existing
/// caller that doesn't send this field keeps today's behavior.
#[derive(Debug, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    #[default]
    Csv,
    Xlsx,
    /// Both files in one ZIP, reusing the same `build_zip` helper
    /// Group Prep's own export already uses — one download instead of
    /// two round trips.
    Both,
}

#[derive(Debug, Deserialize)]
pub struct DedupExportRequest {
    pub session_id: String,
    #[serde(default)]
    pub format: ExportFormat,
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

/// Exports the full report as CSV, xlsx, or both (as a ZIP) — flagged
/// groups first, then typo/name-variant candidates, then related-tenant
/// candidates. See `dedup_export_plan` for the shape both file formats
/// share.
pub async fn export(
    State(state): State<AppState>,
    Json(request): Json<DedupExportRequest>,
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

    let response = match request.format {
        ExportFormat::Csv => build_csv_response(&request.session_id, &report, &records),
        ExportFormat::Xlsx => build_xlsx_response(&request.session_id, &report, &records),
        ExportFormat::Both => build_zip_response(&request.session_id, &report, &records),
    };

    tracing::info!(
        session_id = %request.session_id,
        format = ?request.format,
        flagged_groups = report.flagged_groups.len(),
        typo_variant_candidates = report.typo_variant_candidates.len(),
        related_tenant_candidates = report.related_tenant_candidates.len(),
        export_ms = started.elapsed().as_millis(),
        "Dedup export generated"
    );

    response
}

fn build_csv_response(session_id: &str, report: &DedupReport, records: &[TenantRecord]) -> Response {
    match dedup_csv_export::generate_csv(report, records) {
        Ok(bytes) => file_response(bytes, "text/csv", "duplicate_tenant_check.csv"),
        Err(err) => {
            tracing::error!(session_id = %session_id, error = %err, "Failed generating dedup export CSV");
            internal_error("Failed generating export CSV")
        }
    }
}

fn build_xlsx_response(session_id: &str, report: &DedupReport, records: &[TenantRecord]) -> Response {
    match dedup_xlsx_export::generate_xlsx(report, records) {
        Ok(bytes) => file_response(
            bytes,
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "duplicate_tenant_check.xlsx",
        ),
        Err(err) => {
            tracing::error!(session_id = %session_id, error = %err, "Failed generating dedup export xlsx");
            internal_error("Failed generating export xlsx")
        }
    }
}

fn build_zip_response(session_id: &str, report: &DedupReport, records: &[TenantRecord]) -> Response {
    let csv_bytes = match dedup_csv_export::generate_csv(report, records) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!(session_id = %session_id, error = %err, "Failed generating dedup export CSV");
            return internal_error("Failed generating export CSV");
        }
    };

    let xlsx_bytes = match dedup_xlsx_export::generate_xlsx(report, records) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!(session_id = %session_id, error = %err, "Failed generating dedup export xlsx");
            return internal_error("Failed generating export xlsx");
        }
    };

    let files = vec![
        ExportFile { file_name: "duplicate_tenant_check.csv".to_string(), bytes: csv_bytes },
        ExportFile { file_name: "duplicate_tenant_check.xlsx".to_string(), bytes: xlsx_bytes },
    ];

    match build_zip(files) {
        Ok(bytes) => file_response(bytes, "application/zip", "duplicate_tenant_check.zip"),
        Err(err) => {
            tracing::error!(session_id = %session_id, error = %err, "Failed zipping dedup export files");
            internal_error("Failed generating export ZIP")
        }
    }
}

fn file_response(bytes: Vec<u8>, content_type: &str, file_name: &str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, content_type.parse().unwrap());
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{file_name}\"").parse().unwrap(),
    );
    (headers, bytes).into_response()
}

#[cfg(test)]
#[path = "dedup_tests.rs"]
mod tests;
