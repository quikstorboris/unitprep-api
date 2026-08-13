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
pub struct CorrectGroupRequest {
    pub session_id: String,
    pub group_name: String,

    /// Optional -- an odd/non-dimensioned group (e.g. "Hertz Office
    /// Space") may have neither. When both are given they become the
    /// new name's base ("10x15"); otherwise the group's existing name is
    /// kept as the base and only `additional_properties` (if any) is
    /// appended to it.
    pub width: Option<String>,
    pub length: Option<String>,

    /// Free text appended to the base name if non-empty (e.g. "Ground
    /// Floor", "Climate Controlled").
    pub additional_properties: Option<String>,
}

enum CorrectGroupNotReady {
    Stage(StageError),
    UnknownGroup,
}

fn build_new_group_name(request: &CorrectGroupRequest) -> String {
    let base = request
        .width
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .zip(
            request
                .length
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        )
        .map(|(width, length)| format!("{width}x{length}"))
        .unwrap_or_else(|| request.group_name.clone());

    match request.additional_properties.as_deref().map(str::trim) {
        Some(extra) if !extra.is_empty() => {
            format!("{base} {extra}")
        }
        _ => base,
    }
}

/// Renames every unit currently in `group_name` (across every selected
/// unit file) to a new UnitGroup value built from the submitted
/// dimensions and/or free-text property — a group-wide rename, not a
/// per-unit correction, since a UnitGroup name is by definition shared
/// across many units at once. Implemented as a bulk insert into the same
/// per-unit `corrections` overlay `/correct` uses (see
/// `unitprep_unit_group::apply_corrections`) — one `unitgroup`-field
/// correction per matching unit, all pointing at the same new value —
/// rather than a new storage mechanism.
pub async fn correct_group(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<CorrectGroupRequest>,
) -> Response {
    let new_group_name = build_new_group_name(&request);

    let result = state.unit_group_sessions.with_owned_session_mut(
        &request.session_id,
        user.user_id,
        |session| {
            if let Err(err) = session
                .require_stage(crate::application::unit_group_session::WorkflowStage::Discovered)
            {
                tracing::warn!(
                    session_id = %request.session_id,
                    required = ?err.required,
                    current = ?err.current,
                    "Correct-group called before discovery completed"
                );

                return Err(CorrectGroupNotReady::Stage(err));
            }

            let discovery = session
                .data
                .discovery
                .clone()
                .expect("Discovered stage guarantees discovery data");

            let effective = session.effective_documents();

            let mut matched_any = false;

            // Set when some unit already carries `new_group_name` --
            // distinct from `matched_any` (which unit currently carries
            // the *old* name). Lets a repeat of the exact same rename
            // request be treated as an already-satisfied no-op instead of
            // erroring: the first call renames every matching unit, so a
            // second, identical call finds zero units still under the old
            // name and used to 400 as `unknown_group` even though nothing
            // is actually wrong -- the same request twice should reach the
            // same successful outcome, not fail the second time purely
            // because it already succeeded once.
            let mut already_applied = false;

            for document in effective.iter() {
                if !discovery.unit_file_names.contains(&document.file_name) {
                    continue;
                }

                let Some(unit_group_index) = document.header_index("unitgroup") else {
                    continue;
                };

                let Some(number_index) = document.header_index("number") else {
                    continue;
                };

                for row in &document.rows {
                    let group = row.get(unit_group_index).map(|v| v.trim()).unwrap_or("");

                    if group == new_group_name {
                        already_applied = true;
                    }

                    if group != request.group_name {
                        continue;
                    }

                    // Trimmed to match how the unit number is read
                    // everywhere else it's used as an identifier.
                    let Some(unit_number) = row.get(number_index).map(|v| v.trim().to_string())
                    else {
                        continue;
                    };

                    matched_any = true;

                    session.add_correction(
                        CorrectionKey {
                            file_name: document.file_name.clone(),
                            unit_number,
                            field: "unitgroup".to_string(),
                        },
                        new_group_name.clone(),
                    );
                }
            }

            if !matched_any {
                if already_applied {
                    tracing::info!(
                        session_id = %request.session_id,
                        group_name = %request.group_name,
                        new_group_name = %new_group_name,
                        "Correct-group is a no-op — already renamed to this value"
                    );

                    return run_validation(session, &request.session_id)
                        .map_err(CorrectGroupNotReady::Stage);
                }

                tracing::warn!(
                    session_id = %request.session_id,
                    group_name = %request.group_name,
                    "Correct-group rejected — no unit currently has that UnitGroup value"
                );

                return Err(CorrectGroupNotReady::UnknownGroup);
            }

            tracing::info!(
                session_id = %request.session_id,
                group_name = %request.group_name,
                new_group_name = %new_group_name,
                "Renamed UnitGroup across every matching unit"
            );

            run_validation(session, &request.session_id).map_err(CorrectGroupNotReady::Stage)
        },
    );

    match result {
        Some(Ok(response)) => Json(response).into_response(),

        Some(Err(CorrectGroupNotReady::Stage(err))) => stage_conflict(err),

        Some(Err(CorrectGroupNotReady::UnknownGroup)) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "unknown_group",
                message: "No unit in the selected files currently has that UnitGroup value."
                    .to_string(),
            }),
        )
            .into_response(),

        None => session_not_found(),
    }
}

#[cfg(test)]
#[path = "correct_group_tests.rs"]
mod tests;
