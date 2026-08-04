use axum::{
    extract::{Json, State},
    response::Response,
};
use serde::Deserialize;

use unitprep_core::session_store::SessionStoreExt;

use crate::api::{respond, validate::run_validation, AppState};
use crate::auth::AuthenticatedUser;

#[derive(Debug, Deserialize)]
pub struct AcknowledgeGroupWarningsRequest {
    pub session_id: String,

    /// The warning's own description text (e.g. "Odd UnitGroup values" or
    /// "Rare UnitGroup detected") -- matched directly against
    /// `unitprep_unit_group::{ODD_UNITGROUP, RARE_GROUP}`, the same
    /// constants the check itself raises issues under. An unrecognized
    /// value simply never matches anything in `validate_document`, so
    /// this endpoint doesn't need to separately validate it.
    pub check: String,

    pub group_names: Vec<String>,

    /// `true` to accept every named group "as is" for this one check;
    /// `false` to undo that (the group goes back to being flagged, with
    /// no other change to its data either way).
    pub acknowledged: bool,
}

/// Accepts (or un-accepts) a batch of UnitGroup names "as is" for one
/// specific per-group check -- unlike `/exclude-group(s)`, this changes
/// nothing about the underlying data: the group's units stay exactly as
/// uploaded, they just stop being flagged under *this* check. A group
/// also flagged under the other per-group check is unaffected; that
/// needs its own, separate acknowledgment.
pub async fn acknowledge_group_warnings(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
    Json(request): Json<AcknowledgeGroupWarningsRequest>,
) -> Response {
    let response = state
        .unit_group_sessions
        .with_session_mut(&request.session_id, |session| {
            for group_name in &request.group_names {
                if request.acknowledged {
                    session.acknowledge_group_check(request.check.clone(), group_name.clone());
                } else {
                    session.unacknowledge_group_check(&request.check, group_name);
                }
            }

            tracing::info!(
                session_id = %request.session_id,
                check = %request.check,
                group_count = request.group_names.len(),
                acknowledged = request.acknowledged,
                "Bulk-updated group check acknowledgments"
            );

            run_validation(session, &request.session_id)
        });

    respond(response)
}

#[cfg(test)]
#[path = "acknowledge_group_warnings_tests.rs"]
mod tests;
