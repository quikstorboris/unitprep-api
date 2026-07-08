use std::time::Instant;

use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::{
    api::{session_not_found, AppState},
    application::session_store::SessionStoreExt,
    domain::{
        analysis::{
            analyze_batch,
            build_batch_from_documents,
            load_reference_groups_from_document,
        },
        models::{
            AdvisoryIssue,
            SimilarityMatch,
        },
        session::WorkflowStage,
    },
};

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
    // `None`, which means the session exists but hasn't reached the
    // right stage yet (a business-logic gate, not an expiry).
    let analysis_inputs = match state
        .session_store
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

                    return None;
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

                    return None;
                }

                Some((
                    discovery,
                    session
                        .effective_documents(),
                ))
            },
        ) {
        Some(data) => data,
        None => {
            return session_not_found();
        }
    };

    let Some((
        discovery,
        documents,
    )) = analysis_inputs
    else {
        return empty_response()
            .into_response();
    };

    let unit_docs: Vec<
        &crate::domain::csv_document::CsvDocument,
    > = documents
        .iter()
        .filter(|d| {
            discovery
                .unit_file_names
                .contains(&d.file_name)
        })
        .collect();

    let group_doc = match
        &discovery.selected_group_file_name
    {
        Some(selected_file) => {
            tracing::info!(
                file = %selected_file,
                "Using selected master group file"
            );

            documents
                .iter()
                .find(|d| {
                    d.file_name
                        == *selected_file
                })
        }

        None => {
            documents
                .iter()
                .find(|d| {
                    discovery
                        .group_file_names
                        .contains(
                            &d.file_name,
                        )
                })
        }
    };

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

            return empty_response()
                .into_response();
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

            return empty_response()
                .into_response();
        }
    };

    let _ = state
        .session_store
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

fn empty_response()
-> Json<AnalyzeResponse>
{
    Json(AnalyzeResponse {
        facilities: 0,
        global_groups: 0,
        net_new_groups: 0,
        similar_groups: 0,
        advisory_issues: 0,
        net_new_group_details:
            Vec::new(),
        similar_group_details:
            Vec::new(),
        advisory_issue_details:
            Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::{
        empty_state,
        unit_document,
        validated_state,
    };

    #[tokio::test]
    async fn analyze_returns_404_for_missing_session(
    ) {
        let response = analyze(
            State(empty_state()),
            Json(AnalyzeRequest {
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
    async fn analyze_finds_net_new_groups_with_no_reference_file(
    ) {
        let state = validated_state(
            "s1",
            vec![unit_document(
                "units.csv",
                vec![[
                    "A01",
                    "10x10 Inside Climate",
                    "10",
                    "10",
                ]],
            )],
        );

        let response = analyze(
            State(state),
            Json(AnalyzeRequest {
                session_id: "s1"
                    .to_string(),
            }),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::OK
        );

        let bytes = axum::body::to_bytes(
            response.into_body(),
            usize::MAX,
        )
        .await
        .unwrap();

        let body: serde_json::Value =
            serde_json::from_slice(
                &bytes,
            )
            .unwrap();

        // No master group file was selected, so every group found is
        // net-new by definition (see analyze_batch).
        assert_eq!(
            body["net_new_groups"], 1
        );

        assert_eq!(
            body["net_new_group_details"]
                [0],
            "10x10 Inside Climate"
        );
    }
}