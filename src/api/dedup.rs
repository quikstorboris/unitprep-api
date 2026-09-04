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

use unitprep_core::parsing::parse_document;
use unitprep_core::session_store::SessionStoreExt;
use unitprep_core::uploaded_file::UploadedFile;
use unitprep_core::vendor_format::detect_vendor;
use unitprep_dedup::{DedupReport, TenantRecord};

use crate::api::dedup_view::{build_report_view, DedupReportView};
use crate::api::dropbox_browse::ensure_path_in_root;
use crate::api::{internal_error, session_not_found, ApiErrorBody, AppState};
use crate::application::dedup_session_service::DedupSessionService;
use crate::auth::AuthenticatedUser;
use crate::client_ops::audit_log;
use crate::infrastructure::csv_export::{build_zip, ExportFile};
use crate::infrastructure::{dedup_csv_export, dedup_xlsx_export};

#[derive(Debug, Serialize)]
pub struct DedupCheckResponse {
    pub session_id: String,
    pub report: DedupReportView,
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
    /// The client this check was run for, when the session was opened
    /// from a client's own Dedup tab (`/clients/{clientId}/dedup`) --
    /// `None` for a standalone run with no client context. Recorded on
    /// the Activity Log entry below so "who ran dedup for which client"
    /// is answerable without cross-referencing session ids by hand.
    #[serde(default)]
    pub client_id: Option<uuid::Uuid>,
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
            result = Some(UploadedFile {
                file_name,
                relative_path,
                bytes,
                modified_at: None,
            });
        } else {
            tracing::warn!(
                file = %file_name,
                "Ignoring extra multipart field — duplicate-tenant check takes one file"
            );
        }
    }

    Ok(result)
}

/// The directory portion of a Dropbox path -- `None` for a bare
/// root-level name with no `/` at all (never actually seen in practice:
/// every real file lives at least one level under the configured root).
fn parent_folder(path: &str) -> Option<String> {
    path.rfind('/').map(|i| path[..i].to_string())
}

/// Downloads `path` from Dropbox and wraps it as an `UploadedFile`, the
/// same shape `first_uploaded_file` builds from a multipart field --
/// lets `detect_vendor_format_dropbox`/`import_from_dropbox` reuse the
/// exact same parse/ingest code their local-upload counterparts use,
/// with only the acquisition step differing.
async fn download_as_uploaded_file(
    state: &AppState,
    path: &str,
) -> Result<UploadedFile, Response> {
    let bytes = state.dropbox.download(path).await.map_err(|err| {
        tracing::error!(error = %err, path = %path, "Dropbox download failed during dedup import");
        internal_error("Could not download file from Dropbox")
    })?;

    let file_name = path.rsplit('/').next().unwrap_or(path).to_string();

    Ok(UploadedFile {
        file_name: file_name.clone(),
        relative_path: file_name,
        bytes,
        modified_at: None,
    })
}

/// Uploads and analyzes a QMS export file in one step, creating a new
/// dedup session. Combining upload+analyze (rather than UnitGroup's
/// separate stages) is deliberate: there's no ambiguity to resolve
/// in between, the check just runs.
pub async fn check(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    mut multipart: Multipart,
) -> Response {
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

    // A synchronous read of the in-memory registry snapshot -- see
    // `client_ops::vendor_format`'s module doc comment for why this is
    // never a per-request DB call.
    let tenant_vendors = state.tenant_vendors.read().clone();

    let (session_id, report, records) = match DedupSessionService::new(Arc::clone(
        &state.dedup_sessions,
    ))
    .create_session(file, Some(user.user_id), &tenant_vendors, None)
    {
        Ok(created) => created,
        Err(err) => {
            // A parse/ingest failure here describes a problem with the
            // uploaded file itself (missing FirtLast column, unsupported
            // format, malformed CSV) — a data-quality issue safe to
            // surface directly, not an internal fault.
            tracing::warn!(file = %file_name, error = %err, "Dedup check failed to ingest file");
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

    tracing::info!(
        session_id = %session_id,
        owner_id = %user.user_id,
        file = %file_name,
        flagged_groups = report.flagged_groups.len(),
        typo_variant_candidates = report.typo_variant_candidates.len(),
        check_ms = started.elapsed().as_millis(),
        "Dedup check complete"
    );

    let report = build_report_view(&report, &records);

    Json(DedupCheckResponse { session_id, report }).into_response()
}

#[derive(Debug, Deserialize)]
pub struct DedupDropboxPathRequest {
    pub path: String,
}

/// Dropbox-sourced counterpart to `check()` -- same ingest/session-create
/// logic via `download_as_uploaded_file`, called only after the
/// frontend's confirm-vendor checkbox, exactly like `handleCheck` calls
/// `/dedup/check` today after a local upload.
pub async fn import_from_dropbox(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<DedupDropboxPathRequest>,
) -> Response {
    let started = Instant::now();

    if let Err(response) = ensure_path_in_root(&state, &request.path) {
        return response;
    }

    let file = match download_as_uploaded_file(&state, &request.path).await {
        Ok(file) => file,
        Err(response) => return response,
    };

    let file_name = file.file_name.clone();
    let source_dropbox_folder_path = parent_folder(&request.path);

    // A synchronous read of the in-memory registry snapshot -- see
    // `client_ops::vendor_format`'s module doc comment for why this is
    // never a per-request DB call.
    let tenant_vendors = state.tenant_vendors.read().clone();

    let (session_id, report, records) = match DedupSessionService::new(Arc::clone(
        &state.dedup_sessions,
    ))
    .create_session(file, Some(user.user_id), &tenant_vendors, source_dropbox_folder_path)
    {
        Ok(created) => created,
        Err(err) => {
            tracing::warn!(file = %file_name, error = %err, "Dedup import-from-dropbox failed to ingest file");
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

    tracing::info!(
        session_id = %session_id,
        owner_id = %user.user_id,
        path = %request.path,
        flagged_groups = report.flagged_groups.len(),
        typo_variant_candidates = report.typo_variant_candidates.len(),
        check_ms = started.elapsed().as_millis(),
        "Dedup check complete (imported from Dropbox)"
    );

    let report = build_report_view(&report, &records);

    Json(DedupCheckResponse { session_id, report }).into_response()
}

#[derive(Debug, Serialize)]
pub struct DedupDetectVendorResponse {
    /// `None` when the file doesn't match any registered vendor's
    /// signature — the frontend shows this as "unrecognized" and keeps
    /// Run Check disabled, same outcome `/dedup/check` itself would
    /// reach, just surfaced before the user commits to running it.
    pub vendor_name: Option<String>,
}

/// Parses an uploaded file and reports which registered vendor (if any)
/// its columns match — called as soon as a file is selected, before
/// `/dedup/check` actually runs, so the UI can show "Vendor: {name}" and
/// gate the Run Check button on the user confirming it. Deliberately
/// does not build or store anything: no session, no ingest, no report.
/// `/dedup/check` re-detects the vendor itself when it actually runs
/// rather than trusting this call's result — the same "server
/// re-verifies, never trusts the frontend's own gate" pattern Group
/// Prep's bulk-confirm already uses.
pub async fn detect_vendor_format(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
    mut multipart: Multipart,
) -> Response {
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
            tracing::error!(error = %err, "Multipart parser error during dedup vendor detection");
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

    let document = match parse_document(&file) {
        Ok(document) => document,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiErrorBody {
                    error: "invalid_file",
                    message: format!("Could not read '{}': {err}", file.file_name),
                }),
            )
                .into_response();
        }
    };

    // A synchronous read of the in-memory registry snapshot -- see
    // `client_ops::vendor_format`'s module doc comment for why this is
    // never a per-request DB call.
    let tenant_vendors = state.tenant_vendors.read().clone();

    let vendor_name = detect_vendor(&document, &tenant_vendors).map(|v| v.name.clone());

    Json(DedupDetectVendorResponse { vendor_name }).into_response()
}

/// Dropbox-sourced counterpart to `detect_vendor_format` -- same
/// parse-and-report logic via `download_as_uploaded_file`, source is a
/// Dropbox path instead of a multipart upload. Does not create a
/// session, same as the local-upload version; `/dedup/import-dropbox`
/// re-detects when it actually runs.
pub async fn detect_vendor_format_dropbox(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
    Json(request): Json<DedupDropboxPathRequest>,
) -> Response {
    if let Err(response) = ensure_path_in_root(&state, &request.path) {
        return response;
    }

    let file = match download_as_uploaded_file(&state, &request.path).await {
        Ok(file) => file,
        Err(response) => return response,
    };

    let document = match parse_document(&file) {
        Ok(document) => document,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiErrorBody {
                    error: "invalid_file",
                    message: format!("Could not read '{}': {err}", file.file_name),
                }),
            )
                .into_response();
        }
    };

    // A synchronous read of the in-memory registry snapshot -- see
    // `client_ops::vendor_format`'s module doc comment for why this is
    // never a per-request DB call.
    let tenant_vendors = state.tenant_vendors.read().clone();

    let vendor_name = detect_vendor(&document, &tenant_vendors).map(|v| v.name.clone());

    Json(DedupDetectVendorResponse { vendor_name }).into_response()
}

/// Re-fetches a previously computed report — e.g. after a page refresh,
/// without re-uploading the file.
pub async fn report(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<DedupSessionRequest>,
) -> Response {
    match state
        .dedup_sessions
        .with_owned_session(&request.session_id, user.user_id, |session| {
            (session.report.clone(), session.records.clone())
        }) {
        Some((report, records)) => Json(build_report_view(&report, &records)).into_response(),
        None => session_not_found(&request.session_id),
    }
}

/// The literal name of the auto-created subfolder every Dropbox-sourced
/// export defaults into -- deliberately not derived from the source
/// folder's own name (real facilities call it "Prelim Check", "Final
/// Check", or something else entirely with no consistent convention
/// across clients -- see `DedupSession::source_dropbox_folder_path`'s
/// own doc comment). This name is the one thing OO actually controls.
const DUPLICATE_CHECK_FOLDER_NAME: &str = "Duplicate Check";

#[derive(Debug, Serialize)]
pub struct DedupSaveLocationResponse {
    /// `Some(path)` when this session's source file was imported from
    /// Dropbox -- the `Duplicate Check` subfolder next to wherever that
    /// file actually came from, which the frontend's save-to-Dropbox
    /// picker should default `initialPath` to. `None` for a
    /// locally-uploaded session, which has no Dropbox origin to anchor a
    /// default to; the picker falls back to its own existing behavior.
    pub default_folder_path: Option<String>,
}

/// Computes (but does not yet create -- `export_to_dropbox` creates it
/// at the moment it's actually needed, not speculatively here) this
/// session's default save-to-Dropbox location. Called when the
/// save-to-Dropbox picker opens, so it can seed `initialPath` without
/// the frontend needing to know anything about how that default is
/// derived.
pub async fn save_location(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<DedupSessionRequest>,
) -> Response {
    let source_folder = match state
        .dedup_sessions
        .with_owned_session(&request.session_id, user.user_id, |session| {
            session.source_dropbox_folder_path.clone()
        }) {
        Some(source_folder) => source_folder,
        None => return session_not_found(&request.session_id),
    };

    let default_folder_path =
        source_folder.map(|folder| format!("{folder}/{DUPLICATE_CHECK_FOLDER_NAME}"));

    Json(DedupSaveLocationResponse { default_folder_path }).into_response()
}

/// Exports the full report as CSV, xlsx, or both (as a ZIP) — flagged
/// groups first, then typo/name-variant candidates, then related-tenant
/// candidates. See `dedup_export_plan` for the shape both file formats
/// share.
pub async fn export(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<DedupExportRequest>,
) -> Response {
    let started = Instant::now();

    let session_data = match state.dedup_sessions.with_owned_session(
        &request.session_id,
        user.user_id,
        |session| (session.report.clone(), session.records.clone()),
    ) {
        Some(data) => data,
        None => return session_not_found(&request.session_id),
    };

    let (report, records) = session_data;

    let response = match generate_export(&request.format, &request.session_id, &report, &records)
    {
        Ok((bytes, content_type, file_name)) => {
            audit_log::record(
                &state.db,
                audit_log::event::DEDUP_COMPLETED,
                user.user_id,
                "client",
                request.client_id.as_ref().map(ToString::to_string).as_deref(),
                audit_log::Change::none(),
                None,
                None,
                serde_json::json!({
                    "session_id": request.session_id,
                    "format": format!("{:?}", request.format),
                    "flagged_groups": report.flagged_groups.len(),
                    "typo_variant_candidates": report.typo_variant_candidates.len(),
                    "related_tenant_candidates": report.related_tenant_candidates.len(),
                }),
            )
            .await;
            file_response(bytes, content_type, file_name)
        }
        Err(response) => response,
    };

    tracing::info!(
        session_id = %request.session_id,
        owner_id = %user.user_id,
        format = ?request.format,
        flagged_groups = report.flagged_groups.len(),
        typo_variant_candidates = report.typo_variant_candidates.len(),
        related_tenant_candidates = report.related_tenant_candidates.len(),
        export_ms = started.elapsed().as_millis(),
        "Dedup export generated"
    );

    response
}

#[derive(Debug, Deserialize)]
pub struct DedupExportToDropboxRequest {
    pub session_id: String,
    #[serde(default)]
    pub format: ExportFormat,
    /// Full destination path, filename included -- resolved by the
    /// frontend's Dropbox folder picker plus a client-generated
    /// timestamped filename, not guessed at here.
    pub dropbox_path: String,
    /// Same reasoning as `DedupExportRequest::client_id`.
    #[serde(default)]
    pub client_id: Option<uuid::Uuid>,
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

    if let Err(response) = ensure_path_in_root(&state, &request.dropbox_path) {
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

    let (bytes, _content_type, _default_file_name) =
        match generate_export(&request.format, &request.session_id, &report, &records) {
            Ok(generated) => generated,
            Err(response) => return response,
        };

    // Ensures the destination folder exists before writing to it --
    // covers the `Duplicate Check` subfolder specifically (never created
    // ahead of time, only computed as a suggested path by `save_location`
    // above), but is deliberately unconditional: any destination folder
    // this call is ever pointed at should exist by the time the upload
    // itself is attempted, not just the one this feature was built for.
    if let Some(folder) = parent_folder(&request.dropbox_path) {
        if let Err(err) = state.dropbox.create_folder_if_missing(&folder).await {
            tracing::error!(error = %err, path = %folder, "Dropbox create_folder_if_missing failed during dedup export");
            return internal_error("Could not create the destination folder in Dropbox");
        }
    }

    if let Err(err) = state.dropbox.upload(&request.dropbox_path, bytes).await {
        tracing::error!(error = %err, path = %request.dropbox_path, "Dropbox upload failed during dedup export");
        return internal_error("Could not upload export to Dropbox");
    }

    audit_log::record(
        &state.db,
        audit_log::event::DEDUP_COMPLETED,
        user.user_id,
        "client",
        request.client_id.as_ref().map(ToString::to_string).as_deref(),
        audit_log::Change::none(),
        None,
        None,
        serde_json::json!({
            "session_id": request.session_id,
            "format": format!("{:?}", request.format),
            "dropbox_path": request.dropbox_path,
        }),
    )
    .await;

    tracing::info!(
        session_id = %request.session_id,
        owner_id = %user.user_id,
        format = ?request.format,
        path = %request.dropbox_path,
        export_ms = started.elapsed().as_millis(),
        "Dedup export saved to Dropbox"
    );

    Json(DedupExportToDropboxResponse {
        path: request.dropbox_path,
    })
    .into_response()
}

#[allow(clippy::result_large_err)]
fn generate_csv_bytes(
    session_id: &str,
    report: &DedupReport,
    records: &[TenantRecord],
) -> Result<Vec<u8>, Response> {
    dedup_csv_export::generate_csv(report, records).map_err(|err| {
        tracing::error!(session_id = %session_id, error = %err, "Failed generating dedup export CSV");
        internal_error("Failed generating export CSV")
    })
}

#[allow(clippy::result_large_err)]
fn generate_xlsx_bytes(
    session_id: &str,
    report: &DedupReport,
    records: &[TenantRecord],
) -> Result<Vec<u8>, Response> {
    dedup_xlsx_export::generate_xlsx(report, records).map_err(|err| {
        tracing::error!(session_id = %session_id, error = %err, "Failed generating dedup export xlsx");
        internal_error("Failed generating export xlsx")
    })
}

#[allow(clippy::result_large_err)]
fn generate_zip_bytes(
    session_id: &str,
    report: &DedupReport,
    records: &[TenantRecord],
) -> Result<Vec<u8>, Response> {
    let csv_bytes = generate_csv_bytes(session_id, report, records)?;
    let xlsx_bytes = generate_xlsx_bytes(session_id, report, records)?;

    let files = vec![
        ExportFile {
            file_name: "duplicate_tenant_check.csv".to_string(),
            bytes: csv_bytes,
        },
        ExportFile {
            file_name: "duplicate_tenant_check.xlsx".to_string(),
            bytes: xlsx_bytes,
        },
    ];

    build_zip(files).map_err(|err| {
        tracing::error!(session_id = %session_id, error = %err, "Failed zipping dedup export files");
        internal_error("Failed generating export ZIP")
    })
}

/// Single format-dispatch point shared by `export()` (wraps the result in
/// an HTTP response via `file_response`) and `export_to_dropbox()` (hands
/// the bytes to `state.dropbox.upload` instead) -- the only place that
/// needs to know which generator and content-type/filename go with which
/// `ExportFormat`.
#[allow(clippy::result_large_err)]
fn generate_export(
    format: &ExportFormat,
    session_id: &str,
    report: &DedupReport,
    records: &[TenantRecord],
) -> Result<(Vec<u8>, &'static str, &'static str), Response> {
    match format {
        ExportFormat::Csv => generate_csv_bytes(session_id, report, records)
            .map(|bytes| (bytes, "text/csv", "duplicate_tenant_check.csv")),
        ExportFormat::Xlsx => generate_xlsx_bytes(session_id, report, records).map(|bytes| {
            (
                bytes,
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                "duplicate_tenant_check.xlsx",
            )
        }),
        ExportFormat::Both => generate_zip_bytes(session_id, report, records)
            .map(|bytes| (bytes, "application/zip", "duplicate_tenant_check.zip")),
    }
}

fn file_response(bytes: Vec<u8>, content_type: &str, file_name: &str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, content_type.parse().unwrap());
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{file_name}\"")
            .parse()
            .unwrap(),
    );
    (headers, bytes).into_response()
}

#[cfg(test)]
#[path = "dedup_tests.rs"]
mod tests;
