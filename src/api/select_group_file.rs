use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use unitprep_core::session_store::SessionStoreExt;

use crate::api::{
    discover::compute_discovery,
    session_not_found,
    stage_conflict,
    ApiErrorBody,
    AppState,
};
use crate::application::unit_group_session::{StageError, WorkflowStage};

#[derive(Debug, serde::Deserialize)]
pub struct SelectGroupFileRequest {
    pub session_id: String,
    pub group_file_name: String,
}

enum SelectNotReady {
    Stage(StageError),
    UnknownCandidate,
}

/// Picks one of several auto-discovered group-file candidates -- only
/// meaningful when discovery found more than one (see
/// `DiscoverResponse::group_file_names`); a single candidate is already
/// auto-selected, and this exists purely to break that kind of tie.
/// Distinct from `/group-file/upload`, which introduces a file discovery
/// never found at all. Selecting resets confirmation the same way
/// uploading a different file does -- a freshly picked file needs a
/// fresh "yes, this is the right one" from `/group-file/confirm`.
pub async fn select_group_file(
    State(state): State<AppState>,
    Json(request): Json<SelectGroupFileRequest>,
) -> Response {
    let result = state.unit_group_sessions.with_session_mut(&request.session_id, |session| {
        session
            .require_stage(WorkflowStage::Discovered)
            .map_err(SelectNotReady::Stage)?;

        let is_known_candidate = session
            .data
            .discovery
            .as_ref()
            .expect("Discovered stage guarantees discovery data")
            .group_file_names
            .contains(&request.group_file_name);

        if !is_known_candidate {
            return Err(SelectNotReady::UnknownCandidate);
        }

        // `compute_discovery` re-derives `selected_group_file_name` from
        // this stored value on every call whenever there's more than one
        // candidate (see its own doc comment) -- setting it here on the
        // previous snapshot is what makes the choice stick.
        if let Some(discovery) = session.data.discovery.as_mut() {
            discovery.selected_group_file_name =
                Some(request.group_file_name.clone());
        }

        session.data.group_file_confirmed = false;

        tracing::info!(
            session_id = %request.session_id,
            group_file_name = %request.group_file_name,
            "Master group file selected from multiple candidates"
        );

        Ok(compute_discovery(session))
    });

    match result {
        Some(Ok(response)) => Json(response).into_response(),

        Some(Err(SelectNotReady::Stage(err))) => stage_conflict(err),

        Some(Err(SelectNotReady::UnknownCandidate)) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "unknown_group_file_candidate",
                message: "That file isn't one of the auto-discovered master group file candidates for this session.".to_string(),
            }),
        )
            .into_response(),

        None => session_not_found(),
    }
}

#[cfg(test)]
#[path = "select_group_file_tests.rs"]
mod tests;
