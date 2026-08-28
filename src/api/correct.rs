use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use unitprep_core::session_store::SessionStoreExt;

use crate::api::{
    session_not_found, stage_conflict, validate::run_validation, ApiErrorBody, AppState,
};
use crate::application::unit_group_session::StageError;
use crate::auth::AuthenticatedUser;
use unitprep_unit_group::CorrectionKey;

#[derive(Debug, Deserialize)]
pub struct CorrectRequest {
    pub session_id: String,
    pub file_name: String,
    pub unit_number: String,
    pub field: String,
    pub value: String,
}

enum CorrectNotReady {
    Stage(StageError),

    /// `unit_number` doesn't currently appear in `file_name` at all --
    /// a stale identifier, a typo, or a unit whose group was excluded
    /// since the UI last loaded. Applying the correction anyway would
    /// silently store a dead entry with no effect and no error, per the
    /// same "fail loudly instead" philosophy `correct_group`'s own
    /// `unknown_group` check already applies to a whole-group rename.
    UnknownUnit,

    /// `unit_number` is shared by more than one row in `file_name` --
    /// applying the correction would silently overwrite every one of
    /// those rows identically (see `Session::unit_number_occurrences`'s
    /// doc comment), so it's refused outright rather than guessing which
    /// row the caller meant.
    AmbiguousUnitNumber {
        occurrences: usize,
    },
}

/// Applies one manual correction (e.g. fixing a flagged unit's Width) and
/// immediately re-runs validation, so the caller sees the effect on the
/// error/warning counts without a separate `/validate` round trip. See
/// `Session::effective_documents` for how the correction is layered onto
/// the original parsed data.
pub async fn correct(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<CorrectRequest>,
) -> Response {
    let key = CorrectionKey {
        file_name: request.file_name.clone(),
        unit_number: request.unit_number.clone(),
        field: request.field.to_lowercase(),
    };

    let response = state.unit_group_sessions.with_owned_session_mut(
        &request.session_id,
        user.user_id,
        |session| {
            let occurrences =
                session.unit_number_occurrences(&request.file_name, &request.unit_number);

            if occurrences == 0 {
                tracing::warn!(
                    session_id = %request.session_id,
                    file = %request.file_name,
                    unit_number = %request.unit_number,
                    "Correct rejected — unit number does not currently exist in this file"
                );

                return Err(CorrectNotReady::UnknownUnit);
            }

            if occurrences >= 2 {
                tracing::warn!(
                    session_id = %request.session_id,
                    file = %request.file_name,
                    unit_number = %request.unit_number,
                    occurrences,
                    "Correct rejected — unit number is ambiguous (shared by multiple rows) in this file"
                );

                return Err(CorrectNotReady::AmbiguousUnitNumber { occurrences });
            }

            session.add_correction(key, request.value.clone());

            tracing::info!(
                session_id = %request.session_id,
                file = %request.file_name,
                unit_number = %request.unit_number,
                field = %request.field,
                "Applied manual correction"
            );

            run_validation(session, &request.session_id).map_err(CorrectNotReady::Stage)
        },
    );

    match response {
        Some(Ok(response)) => Json(response).into_response(),

        Some(Err(CorrectNotReady::Stage(err))) => stage_conflict(&request.session_id, err),

        Some(Err(CorrectNotReady::UnknownUnit)) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "unknown_unit",
                message: format!(
                    "No row in '{}' currently has unit number '{}'.",
                    request.file_name, request.unit_number
                ),
            }),
        )
            .into_response(),

        Some(Err(CorrectNotReady::AmbiguousUnitNumber { occurrences })) => (
            StatusCode::CONFLICT,
            Json(ApiErrorBody {
                error: "ambiguous_unit_number",
                message: format!(
                    "{occurrences} rows in this file share unit number '{}' — this correction can't be targeted to a single row. Resolve the duplicate unit numbers in the source file first.",
                    request.unit_number
                ),
            }),
        )
            .into_response(),

        None => session_not_found(&request.session_id),
    }
}

#[cfg(test)]
#[path = "correct_tests.rs"]
mod tests;
