use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{
        discover::compute_discovery,
        session_not_found,
        stage_conflict,
        ApiErrorBody,
        AppState,
    },
    application::unit_group_session::{StageError, WorkflowStage},
};

#[derive(Debug, Deserialize)]
pub struct SelectUnitFileRequest {
    pub session_id: String,
    pub unit_file_name: String,
}

/// Why selection can't proceed — same pattern as `select_group_file`'s
/// `SelectNotReady`.
enum SelectNotReady {
    Stage(StageError),
    FileNotDiscovered,
}

pub async fn select_unit_file(
    State(state): State<AppState>,
    Json(request): Json<SelectUnitFileRequest>,
) -> Response {
    let result = state
        .unit_group_sessions
        .with_session_mut(
            &request.session_id,
            |session| {
                session
                    .require_stage(WorkflowStage::Discovered)
                    .map_err(SelectNotReady::Stage)?;

                let discovery = session
                    .data
                    .discovery
                    .as_ref()
                    .expect("Discovered stage guarantees discovery data");

                if !discovery
                    .unit_file_candidates
                    .iter()
                    .any(|c| c.file_name == request.unit_file_name)
                {
                    return Err(SelectNotReady::FileNotDiscovered);
                }

                let mut discovery = discovery.clone();
                discovery.selected_unit_file_name =
                    Some(request.unit_file_name.clone());
                session.complete_discovery(discovery);

                tracing::info!(
                    session_id = %request.session_id,
                    unit_file_name = %request.unit_file_name,
                    "Unit file selected"
                );

                Ok(compute_discovery(session))
            },
        );

    match result {
        Some(Ok(response)) => Json(response).into_response(),
        Some(Err(SelectNotReady::Stage(err))) => stage_conflict(err),
        Some(Err(SelectNotReady::FileNotDiscovered)) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "unit_file_invalid",
                message: format!(
                    "'{}' was not found among this session's discovered unit file candidates.",
                    request.unit_file_name
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
