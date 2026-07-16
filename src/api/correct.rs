use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use unitprep_core::session_store::SessionStoreExt;

use crate::api::{
    session_not_found,
    stage_conflict,
    validate::run_validation,
    AppState,
};
use unitprep_unit_group::CorrectionKey;

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
        .unit_group_sessions
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
#[path = "correct_tests.rs"]
mod tests;
