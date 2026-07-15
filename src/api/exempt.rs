use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{
        session_not_found,
        stage_conflict,
        validate::run_validation,
        AppState,
    },
    domain::corrections::DimensionExemptionKey,
};

#[derive(Debug, Deserialize)]
pub struct ExemptDimensionsRequest {
    pub session_id: String,
    pub file_name: String,
    pub unit_number: String,
}

/// Marks one unit as intentionally non-dimensioned (an office, an
/// owner's apartment, etc.) so the "Invalid dimensions" check stops
/// flagging its blank/zero Width or Length — an exemption, not a
/// corrected value. Immediately re-runs validation, mirroring `/correct`.
pub async fn exempt_dimensions(
    State(state): State<AppState>,
    Json(request): Json<
        ExemptDimensionsRequest,
    >,
) -> Response {
    let key = DimensionExemptionKey {
        file_name: request
            .file_name
            .clone(),
        unit_number: request
            .unit_number
            .clone(),
    };

    let response = state
        .unit_group_sessions
        .with_session_mut(
            &request.session_id,
            |session| {
                session
                    .add_dimension_exemption(
                        key,
                    );

                tracing::info!(
                    session_id = %request.session_id,
                    file = %request.file_name,
                    unit_number = %request.unit_number,
                    "Exempted unit from dimension validation"
                );

                run_validation(
                    session,
                    &request.session_id,
                )
            },
        );

    match response {
        Some(Ok(response)) => {
            Json(response).into_response()
        }

        Some(Err(err)) => {
            stage_conflict(err)
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
        uploaded_state,
    };

    #[tokio::test]
    async fn exempt_dimensions_returns_404_for_missing_session(
    ) {
        let response =
            exempt_dimensions(
                State(empty_state()),
                Json(
                    ExemptDimensionsRequest {
                        session_id:
                            "missing"
                                .to_string(),
                        file_name:
                            "units.csv"
                                .to_string(),
                        unit_number:
                            "Office"
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

    /// Regression test for the stage/error inconsistency fix:
    /// `/exempt-dimensions` re-runs validation internally, so it must
    /// surface the same 409 (not a fake 200) when the session hasn't
    /// been discovered yet.
    #[tokio::test]
    async fn exempt_dimensions_returns_409_when_called_before_discovery(
    ) {
        let state = uploaded_state(
            "s1",
            vec![unit_document(
                "units.csv",
                vec![[
                    "Office",
                    "10x10 Inside Climate",
                    "",
                    "",
                ]],
            )],
        );

        let response =
            exempt_dimensions(
                State(state),
                Json(
                    ExemptDimensionsRequest {
                        session_id:
                            "s1"
                                .to_string(),
                        file_name:
                            "units.csv"
                                .to_string(),
                        unit_number:
                            "Office"
                                .to_string(),
                    },
                ),
            )
            .await;

        assert_eq!(
            response.status(),
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn exempt_dimensions_clears_error_without_touching_row_data(
    ) {
        let state = discovered_state(
            "s1",
            vec![unit_document(
                "units.csv",
                vec![[
                    "Office",
                    "1200 sq ft",
                    "",
                    "",
                ]],
            )],
        );

        let response =
            exempt_dimensions(
                State(state),
                Json(
                    ExemptDimensionsRequest {
                        session_id:
                            "s1"
                                .to_string(),
                        file_name:
                            "units.csv"
                                .to_string(),
                        unit_number:
                            "Office"
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
            body["error_count"], 0
        );

        assert_eq!(
            body["ready"], true
        );
    }
}
