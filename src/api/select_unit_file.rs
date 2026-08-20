use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{discover::compute_discovery, session_not_found, stage_conflict, ApiErrorBody, AppState},
    application::unit_group_session::{StageError, WorkflowStage},
    auth::AuthenticatedUser,
};

#[derive(Debug, Deserialize)]
pub struct SelectUnitFileRequest {
    pub session_id: String,
    /// The confirmed set of unit files to process -- every discovered
    /// unit file candidate that survived the user's checkbox selection
    /// (defaults to all of them). Replaces any previous confirmation
    /// wholesale, so calling this again (e.g. via "Return to Unit Files
    /// Selection") is how a changed selection takes effect.
    pub unit_file_names: Vec<String>,
}

/// Why selection can't proceed — same pattern as `select_group_file`'s
/// `SelectNotReady`.
enum SelectNotReady {
    Stage(StageError),
    EmptySelection,
    FileNotDiscovered(String),
}

pub async fn select_unit_file(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<SelectUnitFileRequest>,
) -> Response {
    // See `client_ops::vendor_format`'s module doc comment -- a
    // synchronous read of the cached registry, never a per-request DB
    // call.
    let unit_vendors = state.unit_vendors.read().clone();

    let result = state.unit_group_sessions.with_owned_session_mut(
        &request.session_id,
        user.user_id,
        |session| {
            if let Err(err) = session.require_stage(WorkflowStage::Discovered) {
                tracing::warn!(
                    session_id = %request.session_id,
                    required = ?err.required,
                    current = ?err.current,
                    "Unit-file select called before discovery completed"
                );

                return Err(SelectNotReady::Stage(err));
            }

            if request.unit_file_names.is_empty() {
                tracing::warn!(
                    session_id = %request.session_id,
                    "Unit-file select rejected — empty selection"
                );

                return Err(SelectNotReady::EmptySelection);
            }

            let discovery = session
                .data
                .discovery
                .as_ref()
                .expect("Discovered stage guarantees discovery data");

            for name in &request.unit_file_names {
                if !discovery
                    .unit_file_candidates
                    .iter()
                    .any(|c| &c.file_name == name)
                {
                    tracing::warn!(
                        session_id = %request.session_id,
                        file = %name,
                        "Unit-file select rejected — file was not discovered"
                    );

                    return Err(SelectNotReady::FileNotDiscovered(name.clone()));
                }
            }

            let mut discovery = discovery.clone();
            discovery.selected_unit_file_names = request.unit_file_names.clone();
            session.complete_discovery(discovery);

            // One line per file rather than the whole Vec crammed
            // into a single `unit_file_names=[...]` field -- a real
            // multi-facility folder can select a dozen-plus files at
            // once, each with a long path, and a single-line dump of
            // all of them is unreadable in the raw log. The summary
            // line right after keeps the total greppable/gawkable on
            // its own too.
            for file_name in &request.unit_file_names {
                tracing::info!(
                    session_id = %request.session_id,
                    file = %file_name,
                    "Unit file selected"
                );
            }

            tracing::info!(
                session_id = %request.session_id,
                unit_file_count = request.unit_file_names.len(),
                "Unit file selection complete"
            );

            Ok(compute_discovery(session, &unit_vendors))
        },
    );

    match result {
        Some(Ok(response)) => Json(response).into_response(),
        Some(Err(SelectNotReady::Stage(err))) => stage_conflict(err),
        Some(Err(SelectNotReady::EmptySelection)) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "unit_file_selection_empty",
                message: "At least one unit file must be selected.".to_string(),
            }),
        )
            .into_response(),
        Some(Err(SelectNotReady::FileNotDiscovered(name))) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "unit_file_invalid",
                message: format!(
                    "'{name}' was not found among this session's discovered unit file candidates."
                ),
            }),
        )
            .into_response(),
        None => session_not_found(),
    }
}

#[cfg(test)]
#[path = "select_unit_file_tests.rs"]
mod tests;
