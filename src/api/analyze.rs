use std::time::Instant;

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{
        internal_error,
        session_not_found,
        stage_conflict,
        ApiErrorBody,
        AppState,
    },
    domain::{
        analysis::{
            analyze_batch,
            build_batch_from_documents,
            load_reference_groups_from_document,
            select_group_document,
        },
        models::{
            AdvisoryIssue,
            SimilarityMatch,
        },
        session::{
            StageError,
            WorkflowStage,
        },
    },
};

/// Why `/analyze` isn't ready to run yet — distinct from "session
/// missing" (404) and distinct from each other, so the response can say
/// specifically what's needed instead of collapsing both into one vague
/// "not ready" state.
enum AnalyzeNotReady {
    Stage(StageError),
    GroupFileNotSelected,
}

#[derive(Debug, Deserialize)]
pub struct AnalyzeRequest {
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct AnalyzeResponse {
    pub facilities: usize,
    pub global_groups: usize,
    pub net_new_groups: usize,
    pub similar_groups: usize,
    pub advisory_issues: usize,
    pub net_new_group_details:
        Vec<String>,
    pub similar_group_details:
        Vec<SimilarityMatch>,
    pub advisory_issue_details:
        Vec<AdvisoryIssue>,
}

pub async fn analyze(
    State(state): State<AppState>,
    Json(request): Json<AnalyzeRequest>,
) -> Response {
    let started = Instant::now();

    // `with_session`'s own `None` means the session itself doesn't exist
    // (expired or invalid id) — distinct from the closure returning
    // `Err`, which means the session exists but isn't ready for a
    // business-logic reason (wrong stage, or ambiguous group file).
    let analysis_inputs = match state
        .unit_group_sessions
        .with_session(
            &request.session_id,
            |session| {
                if let Err(err) =
                    session.require_stage(
                        WorkflowStage::Validated,
                    )
                {
                    tracing::warn!(
                        session_id = %request.session_id,
                        required = ?err.required,
                        current = ?err.current,
                        "Analyze called before discovery/validation completed"
                    );

                    return Err(AnalyzeNotReady::Stage(err));
                }

                let discovery = session
                    .data
                    .discovery
                    .clone()
                    .expect(
                        "Validated stage guarantees discovery data",
                    );

                if discovery.group_file_names.len() > 1
                    && discovery
                        .selected_group_file_name
                        .is_none()
                {
                    tracing::warn!(
                        session_id = %request.session_id,
                        group_files = ?discovery.group_file_names,
                        "Analysis requires master group file selection"
                    );

                    return Err(AnalyzeNotReady::GroupFileNotSelected);
                }

                Ok((
                    discovery,
                    session
                        .effective_documents(),
                ))
            },
        ) {
        Some(Ok(data)) => data,
        Some(Err(AnalyzeNotReady::Stage(err))) => {
            return stage_conflict(err);
        }
        Some(Err(AnalyzeNotReady::GroupFileNotSelected)) => {
            return (
                StatusCode::CONFLICT,
                Json(ApiErrorBody {
                    error: "group_file_not_selected",
                    message: "Multiple candidate master group files were found; select one via /group-file/select before analyzing.".to_string(),
                }),
            )
                .into_response();
        }
        None => {
            return session_not_found();
        }
    };

    let (discovery, documents) =
        analysis_inputs;

    let unit_docs: Vec<
        &unitprep_core::csv_document::CsvDocument,
    > = documents
        .iter()
        .filter(|d| {
            discovery
                .unit_file_names
                .contains(&d.file_name)
        })
        .collect();

    let group_doc =
        select_group_document(
            &documents,
            &discovery,
        );

    let batch = match build_batch_from_documents(
        unit_docs,
    ) {
        Ok(batch) => batch,

        Err(err) => {
            tracing::error!(
                session_id = %request.session_id,
                error = %err,
                "Failed to build batch"
            );

            return internal_error(
                "Failed to build analysis batch from documents",
            );
        }
    };

    let reference_groups =
        match group_doc {
            Some(doc) => {
                match load_reference_groups_from_document(
                    doc,
                ) {
                    Ok(groups) => {
                        Some(groups)
                    }

                    Err(err) => {
                        tracing::warn!(
                            session_id = %request.session_id,
                            error = %err,
                            "Could not load reference groups"
                        );

                        None
                    }
                }
            }

            None => None,
        };

    let results = match analyze_batch(
        batch,
        reference_groups,
    ) {
        Ok(results) => results,

        Err(err) => {
            tracing::error!(
                session_id = %request.session_id,
                error = %err,
                "Analysis failed"
            );

            return internal_error(
                "Analysis failed",
            );
        }
    };

    let _ = state
        .unit_group_sessions
        .with_session_mut(
            &request.session_id,
            |session| {
                session.complete_analysis(
                    results.clone(),
                );
            },
        );

    tracing::info!(
        session_id = %request.session_id,
        facilities =
            results
                .batch_run
                .facilities
                .len(),
        global_groups =
            results
                .batch_run
                .global_groups
                .len(),
        net_new_groups =
            results
                .net_new_groups
                .len(),
        similar_groups =
            results
                .similar_groups
                .len(),
        advisory_issues =
            results
                .batch_run
                .advisory_issues
                .len(),
        analysis_ms =
            started
                .elapsed()
                .as_millis(),
        "Analysis complete"
    );

    Json(AnalyzeResponse {
        facilities:
            results
                .batch_run
                .facilities
                .len(),

        global_groups:
            results
                .batch_run
                .global_groups
                .len(),

        net_new_groups:
            results
                .net_new_groups
                .len(),

        similar_groups:
            results
                .similar_groups
                .len(),

        advisory_issues:
            results
                .batch_run
                .advisory_issues
                .len(),

        net_new_group_details:
            results
                .net_new_groups
                .clone(),

        similar_group_details:
            results
                .similar_groups
                .clone(),

        advisory_issue_details:
            results
                .batch_run
                .advisory_issues
                .clone(),
    })
    .into_response()
}


#[cfg(test)]
#[path = "analyze_tests.rs"]
mod tests;
