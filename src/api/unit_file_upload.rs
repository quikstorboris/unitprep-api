use axum::extract::{Multipart, State};
use axum::response::{IntoResponse, Response};
use axum::Json;

use unitprep_core::csv_document::CsvDocument;
use unitprep_core::parsing::parse_document;
use unitprep_core::session_store::SessionStoreExt;

use crate::api::manual_file_upload::{
    extract_manual_upload_fields, manual_upload_error_response, ManualUploadError,
};
use crate::api::{discover::compute_discovery, session_not_found, stage_conflict, AppState};
use crate::application::unit_group_session::WorkflowStage;
use crate::auth::AuthenticatedUser;

/// Lets the user manually designate an uploaded file as one of this
/// session's unit files -- for a net-new facility (or any folder) where a
/// real unit list simply doesn't match a known vendor's header signature.
/// Adds it to `selected_unit_file_names` directly, bypassing
/// `detect_vendor` classification; `reconcile_unit_file_selection`'s
/// preservation check only requires the document to still exist, not to
/// independently classify (mirrors `/group-file/upload`). From there the
/// file goes through the same `requires_format_resolution` /
/// `/unit-file/resolve-format` `Map` path every other unit file does --
/// that path was already written to handle "no vendor detected", it just
/// had no way to be reached before this endpoint existed.
pub async fn upload_unit_file(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    multipart: Multipart,
) -> Response {
    let fields = match extract_manual_upload_fields(multipart).await {
        Ok(fields) => fields,
        Err(err) => return manual_upload_error_response(err),
    };

    let document = match parse_document(&fields.file) {
        Ok(document) => document,
        Err(err) => {
            return manual_upload_error_response(ManualUploadError::ParseFailed(format!(
                "Could not read '{}': {err}",
                fields.file.file_name
            )));
        }
    };

    // See `client_ops::vendor_format`'s module doc comment -- a
    // synchronous read of the cached registry, never a per-request DB
    // call.
    let unit_vendors = state.unit_vendors.read().clone();

    apply_unit_file_upload(&state, &fields.session_id, user.user_id, document, &unit_vendors)
}

/// The testable core, separated from the Multipart-extracting handler
/// above so a test can exercise it directly with an already-parsed
/// `CsvDocument`, without constructing a real multipart body.
pub(crate) fn apply_unit_file_upload(
    state: &AppState,
    session_id: &str,
    owner_id: uuid::Uuid,
    document: CsvDocument,
    unit_vendors: &[unitprep_core::vendor_format::VendorFormat],
) -> Response {
    let result =
        state
            .unit_group_sessions
            .with_owned_session_mut(session_id, owner_id, |session| {
                session.require_stage(WorkflowStage::Discovered)?;

                let file_name = document.file_name.clone();

                session.upsert_document(document);

                let mut discovery = session
                    .data
                    .discovery
                    .clone()
                    .expect("Discovered stage guarantees discovery data");

                if !discovery.selected_unit_file_names.contains(&file_name) {
                    discovery.selected_unit_file_names.push(file_name.clone());
                }
                session.data.discovery = Some(discovery);

                // A newly (re)uploaded file needs a fresh mapping even if a
                // stale resolution happens to exist under the same name --
                // same reasoning as the group-file upload resetting
                // `group_file_confirmed`.
                session.data.format_resolutions.remove(&file_name);

                tracing::info!(
                    session_id = %session_id,
                    file_name = %file_name,
                    "Unit file manually uploaded"
                );

                Ok(compute_discovery(session, unit_vendors))
            });

    match result {
        Some(Ok(response)) => Json(response).into_response(),
        Some(Err(err)) => stage_conflict(err),
        None => session_not_found(),
    }
}

#[cfg(test)]
#[path = "unit_file_upload_tests.rs"]
mod tests;
