use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{
        session_not_found,
        validate::run_validation,
        AppState,
    },
    domain::corrections::CorrectionKey,
};

#[derive(Debug, Deserialize)]
pub struct CorrectRequest {
    pub session_id: String,
    pub file_name: String,
    pub unit_number: String,
    pub field: String,
    pub value: String,
}

/// Applies one manual correction (e.g. fixing a flagged unit's Width) and
/// immediately re-runs validation, so the caller sees the effect on the
/// error/warning counts without a separate `/validate` round trip. See
/// `Session::effective_documents` for how the correction is layered onto
/// the original parsed data.
pub async fn correct(
    State(state): State<AppState>,
    Json(request): Json<CorrectRequest>,
) -> Response {
    let key = CorrectionKey {
        file_name: request
            .file_name
            .clone(),
        unit_number: request
            .unit_number
            .clone(),
        field: request
            .field
            .to_lowercase(),
    };

    let response = state
        .session_store
        .with_session_mut(
            &request.session_id,
            |session| {
                session.add_correction(
                    key,
                    request.value.clone(),
                );

                tracing::info!(
                    session_id = %request.session_id,
                    file = %request.file_name,
                    unit_number = %request.unit_number,
                    field = %request.field,
                    "Applied manual correction"
                );

                run_validation(
                    session,
                    &request.session_id,
                )
            },
        );

    match response {
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
    async fn correct_returns_404_for_missing_session(
    ) {
        let response = correct(
            State(empty_state()),
            Json(CorrectRequest {
                session_id: "missing"
                    .to_string(),
                file_name: "units.csv"
                    .to_string(),
                unit_number: "A01"
                    .to_string(),
                field: "width"
                    .to_string(),
                value: "10"
                    .to_string(),
            }),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn correct_clears_invalid_dimensions_error(
    ) {
        // UnitGroup deliberately doesn't parse as a "WxL"-style name —
        // see the comment on the equivalent validate.rs test for why.
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

        // Fix width first — length is still blank, so the error should
        // still be present until both are corrected.
        let response = correct(
            State(state.clone()),
            Json(CorrectRequest {
                session_id: "s1"
                    .to_string(),
                file_name: "units.csv"
                    .to_string(),
                unit_number: "Office"
                    .to_string(),
                field: "width"
                    .to_string(),
                value: "10"
                    .to_string(),
            }),
        )
        .await;

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
            body["error_count"], 1
        );

        // Now fix length too — the error should clear entirely.
        let response = correct(
            State(state),
            Json(CorrectRequest {
                session_id: "s1"
                    .to_string(),
                file_name: "units.csv"
                    .to_string(),
                unit_number: "Office"
                    .to_string(),
                field: "length"
                    .to_string(),
                value: "10"
                    .to_string(),
            }),
        )
        .await;

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
