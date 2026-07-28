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

/// Lets the user manually designate a specific uploaded file as this
/// session's master/reference group file — for the case where none was
/// auto-detected (a folder with a real master file discovery simply
/// didn't recognize) and the user wants explicit control rather than
/// proceeding as a net-new client. `select_group_document` (see
/// `unit-group::analysis::reference`) already prefers
/// `selected_group_file_name` over the auto-classified list whenever
/// it's set, so forcing it here is enough — no other change needed for
/// this file to actually get used during analysis.
pub async fn upload_group_file(State(state): State<AppState>, multipart: Multipart) -> Response {
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

    apply_group_file_upload(&state, &fields.session_id, document)
}

/// The testable core, separated from the Multipart-extracting handler
/// above so a test can exercise it directly with an already-parsed
/// `CsvDocument`, without constructing a real multipart body.
pub(crate) fn apply_group_file_upload(
    state: &AppState,
    session_id: &str,
    document: CsvDocument,
) -> Response {
    let result = state
        .unit_group_sessions
        .with_session_mut(session_id, |session| {
            session.require_stage(WorkflowStage::Discovered)?;

            let file_name = document.file_name.clone();

            session.upsert_document(document);

            let mut discovery = session
                .data
                .discovery
                .clone()
                .expect("Discovered stage guarantees discovery data");

            discovery.selected_group_file_name = Some(file_name.clone());
            session.data.discovery = Some(discovery);

            // A newly (re)selected file hasn't been confirmed yet, even if
            // a previously selected one had been -- "Select Different File"
            // must not silently carry the old confirmation forward onto a
            // file the user hasn't actually looked at yet.
            session.data.group_file_confirmed = false;

            tracing::info!(
                session_id = %session_id,
                file_name = %file_name,
                "Master group file manually uploaded"
            );

            Ok(compute_discovery(session))
        });

    match result {
        Some(Ok(response)) => Json(response).into_response(),
        Some(Err(err)) => stage_conflict(err),
        None => session_not_found(),
    }
}

#[cfg(test)]
#[path = "group_file_upload_tests.rs"]
mod tests;
