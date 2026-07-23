use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{
        discover::compute_discovery,
        session_not_found,
        stage_conflict,
        ApiErrorBody,
        AppState,
    },
    application::unit_group_session::{StageError, WorkflowStage},
};
use unitprep_unit_group::{
    detect_vendor,
    mapping_from_vendor,
    FieldMapping,
    CANONICAL_TARGET_FIELDS,
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
    Map { mapping: Vec<MappingEntryInput> },
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
                session
                    .require_stage(WorkflowStage::Discovered)
                    .map_err(ResolveNotReady::Stage)?;

                let file_name = session
                    .data
                    .discovery
                    .as_ref()
                    .and_then(|d| d.selected_unit_file_name.clone())
                    .ok_or(ResolveNotReady::NoFileSelected)?;

                let document = session
                    .data
                    .documents
                    .iter()
                    .find(|d| d.file_name == file_name)
                    .expect(
                        "a selected unit file name always names a document that was actually discovered",
                    )
                    .clone();

                let mapping: FieldMapping = match request.action {
                    ResolveAction::Confirm => {
                        let vendor = detect_vendor(&document)
                            .ok_or(ResolveNotReady::VendorNotDetected)?;

                        mapping_from_vendor(vendor)
                    }

                    ResolveAction::Map { mapping } => {
                        validate_manual_mapping(&document, &mapping)?
                    }
                };

                session
                    .data
                    .format_resolutions
                    .insert(file_name.clone(), mapping);

                tracing::info!(
                    session_id = %request.session_id,
                    file_name = %file_name,
                    "Unit file format resolved"
                );

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
            !submitted.iter().any(|entry| {
                &entry.target == *required && entry.source.is_some()
            })
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
