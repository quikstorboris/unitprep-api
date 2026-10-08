//! `POST /dedup/check` -- reads the uploaded tenant files and starts a check.

use super::dto::DedupCheckResponse;
use super::session::create_dedup_session;
use crate::api::{bad_request, dedup_blocking, AppState};
use crate::auth::AuthenticatedUser;
use crate::client_ops::tool_runs;
use axum::extract::{Json, Multipart, Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::time::Instant;
use unitprep_core::uploaded_file::UploadedFile;

/// Reads every file field from `multipart` -- the folder flow can check
/// more than one selected file at once. Fields without a filename are
/// skipped.
pub(super) async fn all_uploaded_files(
    multipart: &mut Multipart,
) -> Result<Vec<UploadedFile>, axum::extract::multipart::MultipartError> {
    let mut files = Vec::new();

    while let Some(field) = multipart.next_field().await? {
        let Some(file_name) = field.file_name().map(str::to_string) else {
            continue;
        };
        let relative_path = field.name().unwrap_or(&file_name).to_string();
        let bytes = field.bytes().await?.to_vec();

        files.push(UploadedFile {
            file_name,
            relative_path,
            bytes,
            modified_at: None,
        });
    }

    Ok(files)
}

/// A tool run must always be recorded against a real facility -- see
/// `client_ops::tool_runs`'s own doc comment. `Query` reads the URI via
/// `FromRequestParts`, so it composes cleanly ahead of the
/// body-consuming `Multipart` extractor below with no change needed to
/// `all_uploaded_files`'s field-scanning loop. No `#[serde(default)]`:
/// a request missing `facility_id` fails extraction (400) the same way
/// every other required field in this file already does.
#[derive(Debug, Deserialize)]
pub struct DedupCheckQuery {
    pub facility_id: uuid::Uuid,
}

/// Uploads and analyzes a QMS export file in one step, creating a new
/// dedup session. Combining upload+analyze (rather than UnitGroup's
/// separate stages) is deliberate: there's no ambiguity to resolve
/// in between, the check just runs.
pub async fn check(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Query(query): Query<DedupCheckQuery>,
    mut multipart: Multipart,
) -> Response {
    let started = Instant::now();

    let files = match all_uploaded_files(&mut multipart).await {
        Ok(files) if !files.is_empty() => files,
        Ok(_) => {
            return bad_request("no_file_uploaded", "No file was uploaded".to_string());
        }
        Err(err) => {
            tracing::error!(error = %err, "Multipart parser error during dedup check");
            return bad_request("multipart_error", err.to_string());
        }
    };

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
        None,
    )
    .await
    {
        Ok(result) => result,
        Err(response) => return response,
    };

    let (session_id, report, records, ingested) = match created {
        Ok(created) => created,
        Err(err) => {
            // A parse/recognition/selection failure here describes a
            // problem with the chosen files themselves (unrecognized
            // format, two alternatives picked, unsupported format,
            // malformed CSV) -- a data-quality issue safe to surface
            // directly, not an internal fault.
            tracing::warn!(files = files.len(), error = %err, "Dedup check failed to ingest the selected files");
            return bad_request("invalid_file", err.to_string());
        }
    };

    let file_name = files[ingested].file_name.clone();
    let source_bytes = files[ingested].bytes.clone();

    tracing::info!(
        session_id = %session_id,
        owner_id = %user.user_id,
        file = %file_name,
        flagged_groups = report.flagged_groups.len(),
        typo_variant_candidates = report.typo_variant_candidates.len(),
        check_ms = started.elapsed().as_millis(),
        "Dedup check complete"
    );
    crate::api::slow_operation::warn_if_slow("dedup_check", started.elapsed());

    // The view assembles the whole export plan: CPU-bound, so off the
    // async workers. `records` comes back for the tool-run record below.
    let (report, _, records) = match dedup_blocking::report_view(report, records).await {
        Ok(built) => built,
        Err(response) => return response,
    };

    tool_runs::create_dedup_run(
        &state.db,
        tool_runs::ToolRunCreate {
            facility_id: query.facility_id,
            session_id: &session_id,
            actor_user_id: user.user_id,
            role_keys: &user.role_keys,
            source_file_name: &file_name,
            source_dropbox_path: None,
            source_bytes,
            source_content_type: guess_content_type(&file_name),
            report_summary: serde_json::to_value(&report).unwrap_or_default(),
            records,
        },
    )
    .await;

    Json(DedupCheckResponse { session_id, report }).into_response()
}

/// A content type to store alongside `source_bytes`/`output_bytes` --
/// `UploadedFile` (shared with every other tool) carries no content-type
/// field of its own, so this is a filename-extension guess, same
/// precision the browser's own `accept` attribute already offers on the
/// way in.
pub(super) fn guess_content_type(file_name: &str) -> &'static str {
    let lower = file_name.to_lowercase();

    if lower.ends_with(".xlsx") {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    } else if lower.ends_with(".xls") {
        "application/vnd.ms-excel"
    } else {
        "text/csv"
    }
}
