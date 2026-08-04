use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use unitprep_core::session_store::SessionStoreExt;

use crate::api::{
    discover::compute_discovery, session_not_found, stage_conflict, ApiErrorBody, AppState,
};
use crate::application::unit_group_session::{StageError, WorkflowStage};
use crate::auth::AuthenticatedUser;

#[derive(Debug, serde::Deserialize)]
pub struct ConfirmGroupFileRequest {
    pub session_id: String,
}

enum ConfirmNotReady {
    Stage(StageError),
    NoFileSelected,
    InvalidFormat,
}

/// The explicit "yes, this is the right master group file" step —
/// separate from *selecting* one (auto-detected, or via
/// `/group-file/upload`) so a file sitting there unconfirmed never
/// silently counts as done. Mirrors `/unit-file/resolve-format`'s own
/// select-then-confirm shape.
pub async fn confirm_group_file(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
    Json(request): Json<ConfirmGroupFileRequest>,
) -> Response {
    let result = state.unit_group_sessions.with_session_mut(&request.session_id, |session| {
        if let Err(err) = session.require_stage(WorkflowStage::Discovered) {
            tracing::warn!(
                session_id = %request.session_id,
                required = ?err.required,
                current = ?err.current,
                "Group-file confirm called before discovery completed"
            );

            return Err(ConfirmNotReady::Stage(err));
        }

        let discovery = session
            .data
            .discovery
            .as_ref()
            .expect("Discovered stage guarantees discovery data");

        if discovery.selected_group_file_name.is_none() {
            tracing::warn!(
                session_id = %request.session_id,
                "Group-file confirm rejected — no master group file selected yet"
            );

            return Err(ConfirmNotReady::NoFileSelected);
        }

        // Re-derived fresh here rather than trusting the last computed
        // snapshot -- the same defense-in-depth reasoning as
        // `resolve_unit_format`'s own header-mismatch re-check.
        let file_name = discovery
            .selected_group_file_name
            .clone()
            .expect("checked above");

        let document = session
            .data
            .documents
            .iter()
            .find(|d| d.file_name == file_name)
            .expect("a selected group file name always names a document that was actually discovered");

        if !crate::api::discover::is_group_document(document) {
            tracing::warn!(
                session_id = %request.session_id,
                file_name = %file_name,
                "Group-file confirm rejected — selected file's format is invalid"
            );

            return Err(ConfirmNotReady::InvalidFormat);
        }

        session.data.group_file_confirmed = true;

        tracing::info!(
            session_id = %request.session_id,
            file_name = %file_name,
            "Master group file confirmed"
        );

        Ok(compute_discovery(session))
    });

    match result {
        Some(Ok(response)) => Json(response).into_response(),

        Some(Err(ConfirmNotReady::Stage(err))) => stage_conflict(err),

        Some(Err(ConfirmNotReady::NoFileSelected)) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "no_group_file_selected",
                message: "No master group file has been selected yet — call /group-file/upload first.".to_string(),
            }),
        )
            .into_response(),

        Some(Err(ConfirmNotReady::InvalidFormat)) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "group_file_format_invalid",
                message: "The selected file doesn't have the required columns (Name/Description/Active, or the full Name/Description/Assigned To/Status/Last Updated set) — select a different file.".to_string(),
            }),
        )
            .into_response(),

        None => session_not_found(),
    }
}

#[cfg(test)]
#[path = "group_file_confirm_tests.rs"]
mod tests;
