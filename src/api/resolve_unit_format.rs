use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::Arc;

use crate::api::blocking::with_owned_session_mut_blocking;
use crate::api::{bad_request, conflict};
use crate::{
    api::{
        discover::{
            compute_discovery, current_unit_file_to_resolve, resolve_confirm_action,
            validate_manual_mapping, DiscoverResponse,
        },
        session_not_found, stage_conflict, AppState,
    },
    application::unit_group_session::{Session, StageError, WorkflowStage},
    auth::AuthenticatedUser,
};
use unitprep_core::vendor_format::VendorFormat;

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
    // See `client_ops::vendor_format`'s module doc comment -- a
    // synchronous read of the cached registry, never a per-request DB
    // call.
    let unit_vendors = state.unit_vendors.read().clone();

    let request = Arc::new(request);
    let result = match with_owned_session_mut_blocking(
        "resolve the unit file format",
        &state.unit_group_sessions,
        &request.session_id,
        user.user_id,
        {
            let request = Arc::clone(&request);
            move |session| apply_resolution(session, &request, &unit_vendors)
        },
    )
    .await
    {
        Ok(result) => result,
        Err(response) => return response,
    };

    match result {
        Some(Ok(response)) => Json(response).into_response(),
        Some(Err(err)) => not_ready_response(err, &request.session_id),
        None => session_not_found(&request.session_id),
    }
}

/// What one resolve request does to the session -- confirm the detected
/// vendor, apply a manual mapping, or reset every selected file's
/// resolution -- and the discovery picture afterwards. Runs under the
/// session's write lock, so nothing here awaits.
fn apply_resolution(
    session: &mut Session,
    request: &ResolveUnitFormatRequest,
    unit_vendors: &[VendorFormat],
) -> Result<DiscoverResponse, ResolveNotReady> {
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

        for file_name in &selected_unit_file_names {
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

        return Ok(compute_discovery(session, unit_vendors));
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

    match &request.action {
        ResolveAction::Reset => {
            unreachable!("Reset is handled above, before file_name is resolved")
        }

        ResolveAction::Confirm => {
            resolve_confirm_action(
                session,
                &request.session_id,
                &file_name,
                &document,
                unit_vendors,
            )?;
        }

        ResolveAction::Map { mapping } => {
            let mapping = match validate_manual_mapping(&document, mapping) {
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

    Ok(compute_discovery(session, unit_vendors))
}

/// The response for a request the session was not ready for.
fn not_ready_response(err: ResolveNotReady, session_id: &str) -> Response {
    match err {
        ResolveNotReady::Stage(err) => stage_conflict(session_id, err),

        ResolveNotReady::NoFileSelected => bad_request("no_unit_file_selected", "No unit file has been selected for this session yet — call /unit-file/select first.".to_string()),

        ResolveNotReady::VendorNotDetected => bad_request("vendor_not_detected", "The selected file doesn't match a known vendor format — use \"map\" instead of \"confirm\".".to_string()),

        ResolveNotReady::HeaderMismatch(files) => conflict("unit_file_header_mismatch", format!(
                    "The confirmed unit files don't all share the same columns, so they can't be confirmed as one vendor together. Files that don't match the rest: {}. Return to Unit Files Selection and remove them, or map each file's columns manually.",
                    files.join(", ")
                )),

        ResolveNotReady::UnknownTargetField(target) => bad_request("unknown_target_field", format!(
                    "'{target}' is not one of the canonical target fields."
                )),

        ResolveNotReady::UnknownSourceHeader { target, source } => bad_request("unknown_source_header", format!(
                    "'{source}' (mapped to '{target}') is not a header in the selected file."
                )),

        ResolveNotReady::MissingRequiredFields(fields) => bad_request("mapping_incomplete", format!(
                    "The following required fields must be mapped to a source column: {}.",
                    fields.join(", ")
                )),

    }
}

#[cfg(test)]
#[path = "resolve_unit_format_tests.rs"]
mod tests;
