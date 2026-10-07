//! Creating a dedup session from parsed files.

use super::export_bytes::CreatedDedupSession;
use crate::api::blocking::run_blocking;
use crate::api::AppState;
use crate::application::dedup_session_service::DedupSessionService;
use axum::response::Response;
use std::sync::Arc;
use unitprep_core::uploaded_file::UploadedFile;
use unitprep_core::vendor_format::VendorFormat;
use unitprep_dedup::file_selection::FileFormatMeta;
use uuid::Uuid;

/// Runs `DedupSessionService::create_session` -- which parses every
/// selected file, ingests the records and builds the whole report, all
/// CPU-bound -- on the blocking pool so a large facility cannot stall an
/// async worker (and every unrelated request scheduled on it).
///
/// The files go in by value and come straight back out, since both callers
/// still need the ingested one afterwards (its name and bytes for the
/// tool-run record). The OUTER `Err` is a failure of the work itself (a
/// panic), already turned into the standard 500; the INNER result is the
/// service's own ingest outcome, which callers map to a 400 because it
/// describes a problem with the chosen files.
pub(super) async fn create_dedup_session(
    state: &AppState,
    files: Vec<UploadedFile>,
    owner_id: Option<Uuid>,
    tenant_vendors: Vec<VendorFormat>,
    file_meta: Vec<FileFormatMeta>,
    source_dropbox_folder_path: Option<String>,
) -> Result<(Vec<UploadedFile>, anyhow::Result<CreatedDedupSession>), Response> {
    let service = DedupSessionService::new(Arc::clone(&state.dedup_sessions));

    run_blocking("check the selected files", move || {
        let created = service.create_session(
            &files,
            owner_id,
            &tenant_vendors,
            &file_meta,
            source_dropbox_folder_path,
        );
        (files, created)
    })
    .await
}
