use std::time::Instant;

use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{session_not_found, AppState},
    domain::{
        models::Severity,
        session::{
            Session,
            ValidationIssueSummary,
            ValidationResult,
            WorkflowStage,
        },
        validation::{
            correctable_fields_for,
            is_dimension_exemptable,
            validate_document,
        },
    },
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
    pub ready: bool,
}

pub fn not_discovered_response(
    session_id: &str,
    err: crate::domain::session::StageError,
) -> ValidateResponse {
    tracing::warn!(
        session_id = %session_id,
        required = ?err.required,
        current = ?err.current,
        "Validate called before discovery"
    );

    ValidateResponse {
        files_checked: 0,
        issue_count: 0,
        error_count: 0,
        warning_count: 0,
        issues: Vec::new(),
        ready: false,
    }
}

/// Runs validation against the session's current effective documents
/// (original data plus any manual corrections) and stores the result on
/// the session. Shared by the `/validate` handler and the `/correct`
/// handler — a saved correction re-runs this exact same logic so the
/// caller gets a fresh, consistent `ValidateResponse` either way.
pub fn run_validation(
    session: &mut Session,
    session_id: &str,
) -> ValidateResponse {
    let started = Instant::now();

    if let Err(err) = session
        .require_stage(
            WorkflowStage::Discovered,
        )
    {
        return not_discovered_response(
            session_id,
            err,
        );
    }

    let discovery = session
        .data
        .discovery
        .clone()
        .expect(
            "Discovered stage guarantees discovery data",
        );

    let mut issues = Vec::new();
    let mut error_count = 0usize;
    let mut warning_count = 0usize;
    let mut files_checked = 0;

    let documents =
        session.effective_documents();

    for document in documents.iter() {
        if !discovery
            .unit_file_names
            .contains(&document.file_name)
        {
            continue;
        }

        let exempt_units = session
            .dimension_exemptions_for(
                &document.file_name,
            );

        let document_issues =
            match validate_document(
                document,
                &exempt_units,
            ) {
                Ok(v) => v,
                Err(err) => {
                    tracing::warn!(
                        session_id = %session_id,
                        file = %document.file_name,
                        error = %err,
                        "Validation failed for document"
                    );

                    continue;
                }
            };

        files_checked += 1;

        for issue in document_issues {
            // Severity comes straight from the check that created the
            // issue (see domain::validation) — no re-derivation from
            // `description` text here.
            match issue.severity {
                Severity::Error => {
                    error_count += 1;
                }

                Severity::Warning => {
                    warning_count += 1;
                }

                Severity::Info => {}
            }

            let affected_unit_ids =
                issue.flagged_values;

            let detail = format!(
                "{} unit{}: {}",
                affected_unit_ids.len(),
                if affected_unit_ids.len()
                    == 1
                {
                    ""
                } else {
                    "s"
                },
                affected_unit_ids.join(", "),
            );

            let correctable_fields =
                correctable_fields_for(
                    &issue.description,
                );

            let exemptable =
                is_dimension_exemptable(
                    &issue.description,
                );

            issues.push(
                ValidationIssueSummary {
                    file_name: document
                        .file_name
                        .clone(),
                    severity: issue
                        .severity,
                    description: issue
                        .description,
                    affected_units:
                        affected_unit_ids
                            .len(),
                    affected_unit_ids,
                    detail,
                    correctable_fields,
                    exemptable,
                },
            );
        }
    }

    let validation = ValidationResult {
        files_checked,
        issue_count: issues.len(),
        error_count,
        warning_count,
        issues: issues.clone(),
        ready: error_count == 0,
    };

    session.complete_validation(
        validation.clone(),
    );

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
        ready = validation.ready,
        validation_ms =
            started.elapsed().as_millis(),
        "Validation complete"
    );

    ValidateResponse {
        files_checked: validation
            .files_checked,
        issue_count: validation
            .issue_count,
        error_count: validation
            .error_count,
        warning_count: validation
            .warning_count,
        issues: validation.issues,
        ready: validation.ready,
    }
}

pub async fn validate(
    State(state): State<AppState>,
    Json(request): Json<ValidateRequest>,
) -> Response {
    let response = state
        .session_store
        .with_session_mut(
            &request.session_id,
            |session| {
                run_validation(
                    session,
                    &request.session_id,
                )
            },
        );

    match response {
        Some(response) => {
            Json(response).into_response()
        }

        None => session_not_found(),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::{
        discovered_state,
        empty_state,
        unit_document,
    };

    async fn body_json(
        response: Response,
    ) -> serde_json::Value {
        let bytes = axum::body::to_bytes(
            response.into_body(),
            usize::MAX,
        )
        .await
        .unwrap();

        serde_json::from_slice(&bytes)
            .unwrap()
    }

    #[tokio::test]
    async fn validate_returns_404_for_missing_session(
    ) {
        let response = validate(
            State(empty_state()),
            Json(ValidateRequest {
                session_id: "missing"
                    .to_string(),
            }),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn validate_reports_invalid_dimensions_as_exemptable(
    ) {
        // UnitGroup deliberately doesn't parse as a "WxL"-style name
        // (like the real "1200 sq ft" office repro) — a dimensioned name
        // such as "10x10 Inside Climate" would also trip the *separate*
        // "UnitGroup dimensions do not match Width/Length" check against
        // blank actual values, which isn't what this test is about.
        let state = discovered_state(
            "s1",
            vec![unit_document(
                "units.csv",
                vec![[
                    "Office",
                    "1200 sq ft",
                    "",
                    "",
                ]],
            )],
        );

        let response = validate(
            State(state),
            Json(ValidateRequest {
                session_id: "s1"
                    .to_string(),
            }),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::OK
        );

        let body =
            body_json(response).await;

        assert_eq!(
            body["error_count"], 1
        );

        assert_eq!(
            body["issues"][0]
                ["description"],
            "Invalid dimensions"
        );

        assert_eq!(
            body["issues"][0]
                ["exemptable"],
            true
        );
    }
}
