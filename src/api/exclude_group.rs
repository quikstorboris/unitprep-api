use axum::{
    extract::{Json, State},
    response::Response,
};
use serde::Deserialize;

use unitprep_core::session_store::SessionStoreExt;

use crate::api::{respond, validate::run_validation, AppState};
use crate::auth::AuthenticatedUser;

#[derive(Debug, Deserialize)]
pub struct ExcludeGroupRequest {
    pub session_id: String,
    pub group_name: String,

    /// `true` to exclude the group from every stage downstream of this
    /// point (as if its units were never in the source file at all);
    /// `false` to undo a previous exclusion.
    pub excluded: bool,
}

/// Fully excludes (or restores) one UnitGroup and every unit in it —
/// unlike a correction or a dimension exemption, this removes the rows
/// entirely rather than adjusting one cell or suppressing one check. See
/// `Session::effective_documents`/`filter_excluded_groups` for where the
/// exclusion is actually applied.
pub async fn exclude_group(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<ExcludeGroupRequest>,
) -> Response {
    let response = state.unit_group_sessions.with_owned_session_mut(
        &request.session_id,
        user.user_id,
        |session| {
            if request.excluded {
                session.exclude_group(request.group_name.clone());
            } else {
                session.include_group(&request.group_name);
            }

            tracing::info!(
                session_id = %request.session_id,
                group_name = %request.group_name,
                excluded = request.excluded,
                "Updated group exclusion"
            );

            run_validation(session, &request.session_id)
        },
    );

    respond(response)
}

#[cfg(test)]
#[path = "exclude_group_tests.rs"]
mod tests;
