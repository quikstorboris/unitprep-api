use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{session_not_found, stage_conflict, ApiErrorBody, AppState},
    application::unit_group_session::{StageError, WorkflowStage},
};

#[derive(Debug, Deserialize)]
pub struct SelectGroupFileRequest {
    pub session_id: String,
    pub group_file_name: String,
}

#[derive(Debug, Serialize)]
pub struct SelectGroupFileResponse {
    pub success: bool,
    pub ready: bool,
}

/// Why selection can't proceed — distinct from "session missing" (404)
/// and from each other, same pattern as `analyze::AnalyzeNotReady`.
enum SelectNotReady {
    Stage(StageError),
    FileNotDiscovered,
}

pub async fn select_group_file(
    State(state): State<AppState>,
    Json(request): Json<SelectGroupFileRequest>,
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
                    .as_mut()
                    .expect("Discovered stage guarantees discovery data");

                if !discovery
                    .group_file_names
                    .contains(&request.group_file_name)
                {
                    return Err(SelectNotReady::FileNotDiscovered);
                }

                discovery.selected_group_file_name =
                    Some(
                        request
                            .group_file_name
                            .clone(),
                    );

                discovery.ready =
                    !discovery.unit_file_names.is_empty();

                Ok(SelectGroupFileResponse {
                    success: true,
                    ready: discovery.ready,
                })
            },
        );

    match result {
        Some(Ok(response)) => Json(response).into_response(),
        Some(Err(SelectNotReady::Stage(err))) => stage_conflict(err),
        Some(Err(SelectNotReady::FileNotDiscovered)) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "group_file_invalid",
                message: format!(
                    "'{}' was not found among this session's discovered group files.",
                    request.group_file_name
                ),
            }),
        )
            .into_response(),
        None => session_not_found(),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::{
        discovered_state,
        empty_state,
        unit_document,
    };

    #[tokio::test]
    async fn select_group_file_returns_404_for_missing_session(
    ) {
        let response =
            select_group_file(
                State(empty_state()),
                Json(
                    SelectGroupFileRequest {
                        session_id:
                            "missing"
                                .to_string(),
                        group_file_name:
                            "groups.csv"
                                .to_string(),
                    },
                ),
            )
            .await;

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn select_group_file_rejects_a_file_discovery_never_found(
    ) {
        // `discovered_state` registers its documents as unit files only,
        // so any group_file_name is "not in the discovered list" —
        // exactly the case this test wants (discovery ran, but the
        // requested name isn't one of the group files it actually found).
        let state = discovered_state(
            "s1",
            vec![unit_document(
                "units.csv",
                vec![[
                    "A01",
                    "10x10 Inside Climate",
                    "10",
                    "10",
                ]],
            )],
        );

        let response =
            select_group_file(
                State(state),
                Json(
                    SelectGroupFileRequest {
                        session_id:
                            "s1"
                                .to_string(),
                        group_file_name:
                            "not_discovered.csv"
                                .to_string(),
                    },
                ),
            )
            .await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST
        );

        let bytes = axum::body::to_bytes(
            response.into_body(),
            usize::MAX,
        )
        .await
        .unwrap();

        let body: serde_json::Value =
            serde_json::from_slice(
                &bytes,
            )
            .unwrap();

        assert_eq!(
            body["error"], "group_file_invalid"
        );
    }

    #[tokio::test]
    async fn select_group_file_returns_409_before_discovery_completes() {
        // `empty_state()`'s session (once one exists) starts at
        // `Uploaded`, before `Discovered` — the stage this endpoint
        // actually requires.
        let state = empty_state();
        state.unit_group_sessions.save(crate::application::unit_group_session::Session::new("s1".to_string()));

        let response = select_group_file(
            State(state),
            Json(SelectGroupFileRequest {
                session_id: "s1".to_string(),
                group_file_name: "groups.csv".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
}