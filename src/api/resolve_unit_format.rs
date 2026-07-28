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
            compute_discovery, current_unit_file_to_resolve, find_header_mismatches,
            normalized_headers,
        },
        session_not_found, stage_conflict, ApiErrorBody, AppState,
    },
    application::unit_group_session::{StageError, WorkflowStage},
};
use unitprep_unit_group::{
    detect_vendor, mapping_from_vendor, FieldMapping, CANONICAL_TARGET_FIELDS,
    REQUIRED_TARGET_FIELDS,
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

enum ResolveNotReady {
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
    Json(request): Json<ResolveUnitFormatRequest>,
) -> Response {
    let result = state
        .unit_group_sessions
        .with_session_mut(
            &request.session_id,
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

/// Bulk-confirms a detected vendor across every currently-selected unit
/// file that shares `document`'s exact header shape -- one confirmation
/// resolves all of them at once instead of requiring a click per file
/// (see `find_header_mismatches`, which guarantees every selected file
/// shares this shape before this is ever called). Re-checking each
/// file's own headers here too (rather than just trusting that aggregate
/// result) costs nothing and means a future bug in the aggregate check
/// can't silently resolve a file it shouldn't.
fn resolve_confirm_action(
    session: &mut crate::application::unit_group_session::Session,
    session_id: &str,
    file_name: &str,
    document: &unitprep_core::csv_document::CsvDocument,
) -> Result<(), ResolveNotReady> {
    let vendor = match detect_vendor(document) {
        Some(vendor) => vendor,
        None => {
            tracing::warn!(
                session_id = %session_id,
                file = %file_name,
                "Confirm requested but vendor could not be detected"
            );

            return Err(ResolveNotReady::VendorNotDetected);
        }
    };

    let selected_unit_file_names = session
        .data
        .discovery
        .as_ref()
        .expect("Discovered stage guarantees discovery data")
        .selected_unit_file_names
        .clone();

    let selected_documents: Vec<_> = session
        .data
        .documents
        .iter()
        .filter(|d| selected_unit_file_names.contains(&d.file_name))
        .collect();

    let mismatched = find_header_mismatches(&selected_documents);

    if !mismatched.is_empty() {
        tracing::warn!(
            session_id = %session_id,
            mismatched_files = ?mismatched,
            "Bulk-confirm rejected — selected unit files don't share the same headers"
        );

        return Err(ResolveNotReady::HeaderMismatch(mismatched));
    }

    let mapping = mapping_from_vendor(vendor);
    let current_headers = normalized_headers(document);
    let mut resolved_files = Vec::new();

    for name in &selected_unit_file_names {
        if session.data.format_resolutions.contains_key(name) {
            continue;
        }

        let same_shape = session
            .data
            .documents
            .iter()
            .find(|d| &d.file_name == name)
            .is_some_and(|d| normalized_headers(d) == current_headers);

        if same_shape {
            session
                .data
                .format_resolutions
                .insert(name.clone(), mapping.clone());

            resolved_files.push(name.clone());
        }
    }

    for file_name in &resolved_files {
        tracing::info!(
            session_id = %session_id,
            vendor = vendor.name,
            file = %file_name,
            "Unit file format resolved (bulk-confirmed)"
        );
    }

    tracing::info!(
        session_id = %session_id,
        vendor = vendor.name,
        resolved_file_count = resolved_files.len(),
        "Unit file format bulk-confirm complete"
    );

    Ok(())
}

/// Validates a user-submitted manual mapping against the selected file's
/// own headers, then expands it into a `FieldMapping` covering every
/// canonical target field (unsubmitted targets map to `None`).
fn validate_manual_mapping(
    document: &unitprep_core::csv_document::CsvDocument,
    submitted: &[MappingEntryInput],
) -> Result<FieldMapping, ResolveNotReady> {
    for entry in submitted {
        if !CANONICAL_TARGET_FIELDS.contains(&entry.target.as_str()) {
            return Err(ResolveNotReady::UnknownTargetField(entry.target.clone()));
        }

        if let Some(source) = &entry.source {
            if document.header_index(source).is_none() {
                return Err(ResolveNotReady::UnknownSourceHeader {
                    target: entry.target.clone(),
                    source: source.clone(),
                });
            }
        }
    }

    let missing_required: Vec<String> = REQUIRED_TARGET_FIELDS
        .iter()
        .filter(|required| {
            !submitted
                .iter()
                .any(|entry| &entry.target == *required && entry.source.is_some())
        })
        .map(|s| s.to_string())
        .collect();

    if !missing_required.is_empty() {
        return Err(ResolveNotReady::MissingRequiredFields(missing_required));
    }

    Ok(CANONICAL_TARGET_FIELDS
        .iter()
        .map(|target| {
            let source = submitted
                .iter()
                .find(|entry| entry.target == *target)
                .and_then(|entry| entry.source.clone());

            (target.to_string(), source)
        })
        .collect())
}

#[cfg(test)]
#[path = "resolve_unit_format_tests.rs"]
mod tests;
