use std::time::Instant;

use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{session_not_found, stage_conflict, AppState},
    application::unit_group_session::{Session, StageError, WorkflowStage},
};
use unitprep_unit_group::{
    correctable_fields_for, is_dimension_exemptable, validate_document, FileValidationError,
    GroupCheckAcknowledgments, Severity, ValidationIssueSummary, ValidationResult, ODD_UNITGROUP,
    RARE_GROUP,
};

#[derive(Debug, Deserialize)]
pub struct ValidateRequest {
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct ValidateResponse {
    pub files_checked: usize,
    pub issue_count: usize,
    pub error_count: usize,
    pub warning_count: usize,
    pub issues: Vec<ValidationIssueSummary>,
    pub files_errored: Vec<FileValidationError>,
    pub ready: bool,
}

/// Runs validation against the session's current effective documents
/// (original data plus any manual corrections) and stores the result on
/// the session. Shared by the `/validate` handler and the `/correct`
/// handler — a saved correction re-runs this exact same logic so the
/// caller gets a fresh, consistent `ValidateResponse` either way.
///
/// Returns `Err(StageError)` if the session hasn't reached
/// `WorkflowStage::Discovered` yet — the caller is responsible for
/// turning that into a `stage_conflict` response rather than a fake
/// all-zero success, which is what this used to do directly.
pub fn run_validation(
    session: &mut Session,
    session_id: &str,
) -> Result<ValidateResponse, StageError> {
    let started = Instant::now();

    if let Err(err) = session.require_stage(WorkflowStage::Discovered) {
        tracing::warn!(
            session_id = %session_id,
            required = ?err.required,
            current = ?err.current,
            "Validate called before discovery"
        );

        return Err(err);
    }

    let discovery = session
        .data
        .discovery
        .clone()
        .expect("Discovered stage guarantees discovery data");

    let mut issues = Vec::new();
    let mut error_count = 0usize;
    let mut warning_count = 0usize;
    let mut files_checked = 0;
    let mut files_errored = Vec::new();

    let documents = session.effective_documents();

    // Session-wide, not per-file (a group name is already a session-wide
    // concept the same way `excluded_groups` is) — built once outside the
    // loop rather than per document.
    let group_check_acknowledgments = GroupCheckAcknowledgments {
        odd: session.acknowledged_groups_for(ODD_UNITGROUP),
        rare: session.acknowledged_groups_for(RARE_GROUP),
    };

    for document in documents.iter() {
        if !discovery.unit_file_names.contains(&document.file_name) {
            continue;
        }

        let exempt_units = session.dimension_exemptions_for(&document.file_name);

        // Cheap per-document lookup, built once regardless of how many
        // issues reference it -- lets a per-unit issue's flagged unit
        // numbers be resolved back to the UnitGroup they belong to (see
        // `affected_group_names` below), without `validate_document`
        // itself needing to carry that mapping through its own return
        // type.
        let unit_to_group: std::collections::HashMap<String, String> = match (
            document.header_index("number"),
            document.header_index("unitgroup"),
        ) {
            (Some(unit_idx), Some(group_idx)) => document
                .rows
                .iter()
                .filter_map(|row| {
                    let unit = row.get(unit_idx)?.clone();
                    let group = row.get(group_idx)?.trim().to_string();

                    if unit.is_empty() {
                        None
                    } else {
                        Some((unit, group))
                    }
                })
                .collect(),
            _ => std::collections::HashMap::new(),
        };

        let document_issues =
            match validate_document(document, &exempt_units, &group_check_acknowledgments) {
                Ok(v) => v,
                Err(err) => {
                    // Not a data-quality issue — an internal
                    // inconsistency between discovery's and
                    // validation's own column lookup (see
                    // `validate_document`'s `Err` path). Never let
                    // this look like a clean/absent result: it's
                    // recorded in `files_errored` below, which
                    // `ready` factors in, same as an unresolved
                    // Severity::Error issue does.
                    tracing::error!(
                        session_id = %session_id,
                        file = %document.file_name,
                        error = %err,
                        "Validation failed for document"
                    );

                    files_errored.push(FileValidationError {
                        file_name: document.file_name.clone(),
                        message: err.to_string(),
                    });

                    continue;
                }
            };

        files_checked += 1;

        for issue in document_issues {
            // Severity comes straight from the check that created the
            // issue (see unitprep_unit_group::validation) — no
            // re-derivation from `description` text here.
            match issue.severity {
                Severity::Error => {
                    error_count += 1;
                }

                Severity::Warning => {
                    warning_count += 1;
                }

                Severity::Info => {}
            }

            let mut affected_group_names: Vec<String> = if issue.flagged_are_group_names {
                issue.flagged_values.clone()
            } else {
                issue
                    .flagged_values
                    .iter()
                    .filter_map(|unit| unit_to_group.get(unit).cloned())
                    .filter(|group| !group.is_empty())
                    .collect()
            };

            affected_group_names.sort();
            affected_group_names.dedup();

            let flagged_are_group_names = issue.flagged_are_group_names;

            let group_occurrence_counts = issue.group_occurrence_counts.clone();

            let affected_unit_ids = issue.flagged_values;

            let detail = format!(
                "{} unit{}: {}",
                affected_unit_ids.len(),
                if affected_unit_ids.len() == 1 {
                    ""
                } else {
                    "s"
                },
                affected_unit_ids.join(", "),
            );

            let correctable_fields = correctable_fields_for(&issue.description);

            let exemptable = is_dimension_exemptable(&issue.description);

            issues.push(ValidationIssueSummary {
                file_name: document.file_name.clone(),
                severity: issue.severity,
                description: issue.description,
                affected_units: affected_unit_ids.len(),
                affected_unit_ids,
                detail,
                correctable_fields,
                exemptable,
                affected_group_names,
                flagged_are_group_names,
                group_occurrence_counts,
            });
        }
    }

    let validation = ValidationResult {
        files_checked,
        issue_count: issues.len(),
        error_count,
        warning_count,
        issues: issues.clone(),
        ready: error_count == 0 && files_errored.is_empty(),
        files_errored,
    };

    session.complete_validation(validation.clone());

    tracing::info!(
        session_id = %session_id,
        files_checked =
            validation.files_checked,
        issue_count =
            validation.issue_count,
        error_count =
            validation.error_count,
        warning_count =
            validation.warning_count,
        files_errored_count =
            validation.files_errored.len(),
        ready = validation.ready,
        validation_ms =
            started.elapsed().as_millis(),
        "Validation complete"
    );

    Ok(ValidateResponse {
        files_checked: validation.files_checked,
        issue_count: validation.issue_count,
        error_count: validation.error_count,
        warning_count: validation.warning_count,
        issues: validation.issues,
        files_errored: validation.files_errored,
        ready: validation.ready,
    })
}

pub async fn validate(
    State(state): State<AppState>,
    Json(request): Json<ValidateRequest>,
) -> Response {
    let response = state
        .unit_group_sessions
        .with_session_mut(&request.session_id, |session| {
            run_validation(session, &request.session_id)
        });

    match response {
        Some(Ok(response)) => Json(response).into_response(),

        Some(Err(err)) => stage_conflict(err),

        None => session_not_found(),
    }
}

#[cfg(test)]
#[path = "validate_tests.rs"]
mod tests;
