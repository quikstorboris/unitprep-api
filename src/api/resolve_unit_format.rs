use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{
        discover::{
            compute_discovery, current_unit_file_to_resolve, resolve_confirm_action,
            validate_manual_mapping,
        },
        session_not_found, stage_conflict, ApiErrorBody, AppState,
    },
    application::unit_group_session::{StageError, WorkflowStage},
    auth::AuthenticatedUser,
};

#[derive(Debug, Deserialize)]
pub struct MappingEntryInput {
    pub target: String,
    pub source: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ResolveAction {
    Confirm,
    Map {
        mapping: Vec<MappingEntryInput>,
    },
    /// Clears the stored resolution for every currently-selected unit
    /// file, undoing a previous "confirm" or "map" -- the only way back
    /// into the confirm/map screen once every selected file is already
    /// resolved (see the frontend's "Change Vendor" button). Unlike
    /// `Confirm`/`Map`, this doesn't act on "the current file to
    /// resolve" -- there isn't one once everything's resolved, which is
    /// exactly the state this exists to undo.
    Reset,
}

#[derive(Debug, Deserialize)]
pub struct ResolveUnitFormatRequest {
    pub session_id: String,
    #[serde(flatten)]
    pub action: ResolveAction,
}

pub(crate) enum ResolveNotReady {
    Stage(StageError),
    NoFileSelected,
    VendorNotDetected,
    HeaderMismatch(Vec<String>),
    UnknownTargetField(String),
    UnknownSourceHeader { target: String, source: String },
    MissingRequiredFields(Vec<String>),
}

pub async fn resolve_unit_format(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<ResolveUnitFormatRequest>,
) -> Response {
    let result = state
        .unit_group_sessions
        .with_owned_session_mut(
            &request.session_id,
            user.user_id,
            |session| {
                if let Err(err) = session.require_stage(WorkflowStage::Discovered) {
                    tracing::warn!(
                        session_id = %request.session_id,
                        required = ?err.required,
                        current = ?err.current,
                        "Resolve-unit-format called before discovery completed"
                    );

                    return Err(ResolveNotReady::Stage(err));
                }

                // Handled before looking up "the current file to resolve"
                // -- unlike Confirm/Map, Reset is meant to run exactly
                // when there isn't one (every selected file is already
                // resolved), so it operates on the whole selected set
                // instead.
                if matches!(request.action, ResolveAction::Reset) {
                    let selected_unit_file_names = session
                        .data
                        .discovery
                        .as_ref()
                        .expect("Discovered stage guarantees discovery data")
                        .selected_unit_file_names
                        .clone();

                    for name in &selected_unit_file_names {
                        session.data.format_resolutions.remove(name);
                    }

                    for file_name in
                        &selected_unit_file_names
                    {
                        tracing::info!(
                            session_id = %request.session_id,
                            file = %file_name,
                            "Unit file format resolution reset"
                        );
                    }

                    tracing::info!(
                        session_id = %request.session_id,
                        reset_file_count = selected_unit_file_names.len(),
                        "Unit file format reset complete"
                    );

                    return Ok(compute_discovery(session));
                }

                let file_name = match current_unit_file_to_resolve(session) {
                    Some(name) => name,
                    None => {
                        tracing::warn!(
                            session_id = %request.session_id,
                            "Resolve-unit-format called with no unit file selected"
                        );

                        return Err(ResolveNotReady::NoFileSelected);
                    }
                };

                let document = session
                    .data
                    .documents
                    .iter()
                    .find(|d| d.file_name == file_name)
                    .expect(
                        "the current unit file to resolve always names a document that was actually discovered",
                    )
                    .clone();

                match request.action {
                    ResolveAction::Reset => {
                        unreachable!("Reset is handled above, before file_name is resolved")
                    }

                    ResolveAction::Confirm => {
                        resolve_confirm_action(
                            session,
                            &request.session_id,
                            &file_name,
                            &document,
                        )?;
                    }

                    ResolveAction::Map { mapping } => {
                        let mapping = match validate_manual_mapping(&document, &mapping) {
                            Ok(mapping) => mapping,
                            Err(err) => {
                                tracing::warn!(
                                    session_id = %request.session_id,
                                    file = %file_name,
                                    "Manual unit-file mapping rejected"
                                );

                                return Err(err);
                            }
                        };

                        session
                            .data
                            .format_resolutions
                            .insert(file_name.clone(), mapping);

                        tracing::info!(
                            session_id = %request.session_id,
                            file_name = %file_name,
                            "Unit file format resolved (manual mapping)"
                        );
                    }
                }

                Ok(compute_discovery(session))
            },
        );

    match result {
        Some(Ok(response)) => Json(response).into_response(),

        Some(Err(ResolveNotReady::Stage(err))) => stage_conflict(err),

        Some(Err(ResolveNotReady::NoFileSelected)) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "no_unit_file_selected",
                message: "No unit file has been selected for this session yet — call /unit-file/select first.".to_string(),
            }),
        )
            .into_response(),

        Some(Err(ResolveNotReady::VendorNotDetected)) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "vendor_not_detected",
                message: "The selected file doesn't match a known vendor format — use \"map\" instead of \"confirm\".".to_string(),
            }),
        )
            .into_response(),

        Some(Err(ResolveNotReady::HeaderMismatch(files))) => (
            StatusCode::CONFLICT,
            Json(ApiErrorBody {
                error: "unit_file_header_mismatch",
                message: format!(
                    "The confirmed unit files don't all share the same columns, so they can't be confirmed as one vendor together. Files that don't match the rest: {}. Return to Unit Files Selection and remove them, or map each file's columns manually.",
                    files.join(", ")
                ),
            }),
        )
            .into_response(),

        Some(Err(ResolveNotReady::UnknownTargetField(target))) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "unknown_target_field",
                message: format!(
                    "'{target}' is not one of the canonical target fields."
                ),
            }),
        )
            .into_response(),

        Some(Err(ResolveNotReady::UnknownSourceHeader { target, source })) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "unknown_source_header",
                message: format!(
                    "'{source}' (mapped to '{target}') is not a header in the selected file."
                ),
            }),
        )
            .into_response(),

        Some(Err(ResolveNotReady::MissingRequiredFields(fields))) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "mapping_incomplete",
                message: format!(
                    "The following required fields must be mapped to a source column: {}.",
                    fields.join(", ")
                ),
            }),
        )
            .into_response(),

        None => session_not_found(),
    }
}

#[cfg(test)]
#[path = "resolve_unit_format_tests.rs"]
mod tests;
