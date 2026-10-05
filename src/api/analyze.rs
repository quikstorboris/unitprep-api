use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::blocking::{run_blocking, with_owned_session_blocking},
    api::{internal_error, session_not_found, stage_conflict, ApiErrorBody, AppState},
    application::unit_group_session::{StageError, WorkflowStage},
    auth::AuthenticatedUser,
    client_ops::tool_runs,
};
use unitprep_unit_group::{
    analyze_batch, build_batch_from_documents, load_reference_groups_from_document,
    select_group_document, AdvisoryIssue, SimilarityMatch,
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
    /// The facility this run is for, when it was opened from a facility's
    /// own Unit Groups page. When present the analysis is recorded on the
    /// facility's Onboarding Work page.
    #[serde(default)]
    pub facility_id: Option<uuid::Uuid>,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct AnalyzeResponse {
    pub facilities: usize,
    pub global_groups: usize,
    pub net_new_groups: usize,
    pub similar_groups: usize,
    pub advisory_issues: usize,
    pub net_new_group_details: Vec<String>,
    pub similar_group_details: Vec<SimilarityMatch>,
    pub advisory_issue_details: Vec<AdvisoryIssue>,
}

pub async fn analyze(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<AnalyzeRequest>,
) -> Response {
    let started = Instant::now();

    // `with_owned_session`'s own `None` means the session itself doesn't
    // exist, is cancelled, or belongs to a different owner (indistinguishable
    // on purpose, see core::session_store) — distinct from the closure
    // returning `Err`, which means the session exists and is this caller's
    // but isn't ready for a business-logic reason (wrong stage, or
    // ambiguous group file).
    // `effective_documents_for` clones and transforms every relevant
    // document under the session's read lock: CPU-bound, so off the async
    // workers. `request` is shared into the closure through an `Arc` so
    // its body reads `request.field` exactly as it always did.
    let request = Arc::new(request);
    let read = match with_owned_session_blocking(
        "prepare the analysis",
        &state.unit_group_sessions,
        &request.session_id,
        user.user_id,
        {
            let request = Arc::clone(&request);
            move |session| {
                if let Err(err) = session.require_stage(WorkflowStage::Validated) {
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
                    .expect("Validated stage guarantees discovery data");

                if discovery.group_file_names.len() > 1
                    && discovery.selected_group_file_name.is_none()
                {
                    tracing::warn!(
                        session_id = %request.session_id,
                        group_files = ?discovery.group_file_names,
                        "Analysis requires master group file selection"
                    );

                    return Err(AnalyzeNotReady::GroupFileNotSelected);
                }

                // Only transform (map/correct/exclude) the documents
                // this call can actually use — the confirmed unit files,
                // plus every group-file candidate `select_group_document`
                // below might look up — instead of every document ever
                // uploaded to the session, which can include stray or
                // superseded files nothing here reads.
                let relevant_names: Vec<String> = discovery
                    .unit_file_names
                    .iter()
                    .cloned()
                    .chain(discovery.group_file_names.iter().cloned())
                    .collect();

                Ok((
                    discovery,
                    session.effective_documents_for(&relevant_names),
                    session.data_generation(),
                ))
            }
        },
    )
    .await
    {
        Ok(read) => read,
        Err(response) => return response,
    };

    let analysis_inputs = match read {
        Some(Ok(data)) => data,
        Some(Err(AnalyzeNotReady::Stage(err))) => {
            return stage_conflict(&request.session_id, err);
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
            return session_not_found(&request.session_id);
        }
    };

    let (discovery, documents, read_generation) = analysis_inputs;

    // Building the batch and running the analysis is the heavy part: off
    // the async workers. `discovery` goes in and comes back out for the
    // tool-run record below.
    let computed = match run_blocking("analyze the session", {
        let request = Arc::clone(&request);
        move || {
            let unit_docs: Vec<&unitprep_core::csv_document::CsvDocument> = documents
                .iter()
                .filter(|d| discovery.unit_file_names.contains(&d.file_name))
                .collect();

            let group_doc = select_group_document(&documents, &discovery);

            let batch = match build_batch_from_documents(unit_docs) {
                Ok(batch) => batch,

                Err(err) => {
                    tracing::error!(
                        session_id = %request.session_id,
                        error = %err,
                        "Failed to build batch"
                    );

                    return Err("Failed to build analysis batch from documents");
                }
            };

            let reference_groups = match group_doc {
                Some(doc) => match load_reference_groups_from_document(doc) {
                    Ok(groups) => Some(groups),

                    Err(err) => {
                        tracing::warn!(
                            session_id = %request.session_id,
                            error = %err,
                            "Could not load reference groups"
                        );

                        None
                    }
                },

                None => None,
            };

            match analyze_batch(batch, reference_groups) {
                Ok(results) => Ok((results, discovery)),

                Err(err) => {
                    tracing::error!(
                        session_id = %request.session_id,
                        error = %err,
                        "Analysis failed"
                    );

                    Err("Analysis failed")
                }
            }
        }
    })
    .await
    {
        Ok(computed) => computed,
        Err(response) => return response,
    };

    let (results, discovery) = match computed {
        Ok(computed) => computed,
        Err(message) => return internal_error(message),
    };

    // Arc, not owned -- storing this on the session and reading its
    // fields below to build the response should share one allocation,
    // not each deep-clone the batch's facilities/groups/issues.
    let results = Arc::new(results);

    match state.unit_group_sessions.with_owned_session_mut(
        &request.session_id,
        user.user_id,
        |session| {
            // A correction/exemption/exclusion/acknowledgment landing in
            // the gap between the read above and this write-back already
            // downgraded `workflow` back to `Validated` as its own safety
            // net (see `run_validation` -> `complete_validation`) —
            // unconditionally calling `complete_analysis` here would
            // silently re-promote it to `Analyzed` using `results`
            // computed from data that's no longer current. Comparing the
            // generation captured at read time catches exactly that.
            if session.data_generation() == read_generation {
                session.complete_analysis(results.clone());
                true
            } else {
                false
            }
        },
    ) {
        Some(true) => {}

        Some(false) => {
            tracing::warn!(
                session_id = %request.session_id,
                "Session data changed during analysis — discarding the stale write-back so the workflow stage can't be falsely re-promoted to Analyzed"
            );
        }

        None => {
            // The session was deleted/expired in the narrow window
            // between the read lock above and this write-back. The
            // analysis itself is already complete and valid — there's
            // nothing to recover by erroring here, since no later call
            // could have used the advanced stage anyway. This just makes
            // a previously-silent race observable instead of changing
            // the response.
            tracing::warn!(
                session_id = %request.session_id,
                "Session no longer exists — analysis stage could not be recorded"
            );
        }
    }

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

    let response = AnalyzeResponse {
        facilities: results.batch_run.facilities.len(),

        global_groups: results.batch_run.global_groups.len(),

        net_new_groups: results.net_new_groups.len(),

        similar_groups: results.similar_groups.len(),

        advisory_issues: results.batch_run.advisory_issues.len(),

        net_new_group_details: results.net_new_groups.clone(),

        similar_group_details: results.similar_groups.clone(),

        advisory_issue_details: results.batch_run.advisory_issues.clone(),
    };

    if let Some(facility_id) = request.facility_id {
        let unit_files: Vec<String> = discovery
            .unit_file_names
            .iter()
            .map(|name| display_name(name))
            .collect();
        let group_file = discovery
            .selected_group_file_name
            .as_deref()
            .or(discovery.group_file_names.first().map(String::as_str))
            .map(display_name);

        tool_runs::record_run(
            &state.db,
            tool_runs::RunRecord {
                tool: "unit_group",
                facility_id,
                session_id: &request.session_id,
                actor_user_id: user.user_id,
                role_keys: &user.role_keys,
                source_file_name: &source_label(&unit_files),
                report_summary: unit_group_summary(&response, unit_files, group_file),
            },
        )
        .await;
    }

    Json(response).into_response()
}

/// The last path segment of a file name, for display.
fn display_name(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).to_string()
}

/// What the run's `source_file_name` column shows: the one unit file, or
/// the first plus a count when there are several.
fn source_label(unit_files: &[String]) -> String {
    match unit_files {
        [] => "Unit files".to_string(),
        [only] => only.clone(),
        [first, rest @ ..] => format!("{first} + {} more", rest.len()),
    }
}

/// The stored summary for a Unit Groups run: the analysis response plus
/// which files it read.
fn unit_group_summary(
    response: &AnalyzeResponse,
    unit_files: Vec<String>,
    group_file: Option<String>,
) -> serde_json::Value {
    let mut summary = serde_json::to_value(response).unwrap_or_default();
    if let Some(object) = summary.as_object_mut() {
        object.insert("unit_files".to_string(), serde_json::json!(unit_files));
        object.insert("group_file".to_string(), serde_json::json!(group_file));
    }
    summary
}

#[cfg(test)]
#[path = "analyze_tests.rs"]
mod tests;
