use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{session_not_found, AppState},
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

pub async fn select_group_file(
    State(state): State<AppState>,
    Json(request): Json<SelectGroupFileRequest>,
) -> Response {
    let result = state
        .session_store
        .with_session_mut(
            &request.session_id,
            |session| {
                let discovery =
                    match session.data.discovery.as_mut()
                {
                    Some(d) => d,
                    None => {
                        return SelectGroupFileResponse {
                            success: false,
                            ready: false,
                        };
                    }
                };

                if !discovery
                    .group_file_names
                    .contains(&request.group_file_name)
                {
                    return SelectGroupFileResponse {
                        success: false,
                        ready: false,
                    };
                }

                discovery.selected_group_file_name =
                    Some(
                        request
                            .group_file_name
                            .clone(),
                    );

                discovery.ready =
                    !discovery.unit_file_names.is_empty();

                SelectGroupFileResponse {
                    success: true,
                    ready: true,
                }
            },
        );

    match result {
        Some(response) => {
            Json(response).into_response()
        }
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
            StatusCode::OK
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
            body["success"], false
        );
    }
}