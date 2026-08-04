use axum::{
    extract::{Json, State},
    response::Response,
};
use serde::Deserialize;

use unitprep_core::session_store::SessionStoreExt;

use crate::api::{respond, validate::run_validation, AppState};
use crate::auth::AuthenticatedUser;

#[derive(Debug, Deserialize)]
pub struct ExcludeGroupsRequest {
    pub session_id: String,
    pub group_names: Vec<String>,

    /// `true` to exclude every named group; `false` to restore all of
    /// them. Same semantics as `/exclude-group`, just applied to a batch.
    pub excluded: bool,
}

/// Bulk form of `/exclude-group` -- excludes (or restores) every named
/// UnitGroup in one request instead of one round-trip per group, for the
/// "Exclude All" action on a warning section's review list (some real
/// files have dozens of rare/odd groups, one-by-one would be tedious).
/// Runs validation once at the end, not once per group.
pub async fn exclude_groups(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
    Json(request): Json<ExcludeGroupsRequest>,
) -> Response {
    let response = state
        .unit_group_sessions
        .with_session_mut(&request.session_id, |session| {
            for group_name in &request.group_names {
                if request.excluded {
                    session.exclude_group(group_name.clone());
                } else {
                    session.include_group(group_name);
                }
            }

            tracing::info!(
                session_id = %request.session_id,
                group_count = request.group_names.len(),
                excluded = request.excluded,
                "Bulk-updated group exclusions"
            );

            run_validation(session, &request.session_id)
        });

    respond(response)
}

#[cfg(test)]
#[path = "exclude_groups_tests.rs"]
mod tests;
