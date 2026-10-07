//! Download exports: where the saved file goes, and `POST /dedup/export`.

use super::dto::{DedupExportRequest, DedupSessionRequest};
use super::export_bytes::{compute_export_file_names, file_response};
use crate::api::{dedup_blocking, session_not_found, AppState};
use crate::auth::AuthenticatedUser;
use crate::client_ops::{audit_log, tool_runs};
use axum::extract::{Json, State};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::time::Instant;
use unitprep_core::session_store::SessionStoreExt;

/// The literal name of the auto-created subfolder every Dropbox-sourced
/// export defaults into -- deliberately not derived from the source
/// folder's own name (real facilities call it "Prelim Check", "Final
/// Check", or something else entirely with no consistent convention
/// across clients -- see `DedupSession::source_dropbox_folder_path`'s
/// own doc comment). This name is the one thing OO actually controls.
pub(super) const DUPLICATE_CHECK_FOLDER_NAME: &str = "Duplicate Check";

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
    let source_folder = match state.dedup_sessions.with_owned_session(
        &request.session_id,
        user.user_id,
        |session| session.source_dropbox_folder_path.clone(),
    ) {
        Some(source_folder) => source_folder,
        None => return session_not_found(&request.session_id),
    };

    let default_folder_path =
        source_folder.map(|folder| format!("{folder}/{DUPLICATE_CHECK_FOLDER_NAME}"));

    Json(DedupSaveLocationResponse {
        default_folder_path,
    })
    .into_response()
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

    let file_names = match compute_export_file_names(
        &state.db,
        &user,
        request.facility_id,
        &request.format,
        false,
    )
    .await
    {
        Ok(file_names) => file_names,
        Err(response) => return response,
    };

    let (bytes, content_type, report, _records) = match dedup_blocking::export(
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

    tool_runs::attach_output_bytes(
        &state.db,
        user.user_id,
        &user.role_keys,
        &request.session_id,
        bytes.clone(),
        content_type,
        &file_names.outer,
    )
    .await;

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
            "flagged_groups": report.flagged_groups.len(),
            "duplicate_customer_records": report.duplicate_customer_records.len(),
            "typo_variant_candidates": report.typo_variant_candidates.len(),
            "related_tenant_candidates": report.related_tenant_candidates.len(),
        }),
    )
    .await;
    let response = file_response(bytes, content_type, &file_names.outer);

    tracing::info!(
        session_id = %request.session_id,
        owner_id = %user.user_id,
        format = ?request.format,
        flagged_groups = report.flagged_groups.len(),
        duplicate_customer_records = report.duplicate_customer_records.len(),
        typo_variant_candidates = report.typo_variant_candidates.len(),
        related_tenant_candidates = report.related_tenant_candidates.len(),
        export_ms = started.elapsed().as_millis(),
        "Dedup export generated"
    );

    response
}
