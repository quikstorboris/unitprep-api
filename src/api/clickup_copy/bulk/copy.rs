//! `bulk-copy`: validates the request and loads its context, then posts the
//! comment to the chosen destinations -- inside the request when it is
//! small, otherwise as a background job (see `runner`).

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::super::exec::{
    copy_chunk, estimated_calls, BulkItem, ItemResult, SourceLink, CHUNK_SIZE, INLINE_CALL_BUDGET,
};
use super::super::jobs;
use super::super::lists::{
    load_tasks_lenient, valid_task_id, ListInfo, MAX_COMMENT_CHARS, PERMISSION,
};
use super::context::prepare_bulk;
use super::runner::{finish_copy, run_job};
use crate::api::clickup_connection::{clickup_client, load_user_token};
use crate::api::rls::try_response;
use crate::api::{bad_request, conflict, internal_error, user_agent_from, AppState};
use crate::auth::AuthenticatedUser;
use crate::clickup::rate_limit;

/// Destinations in one bulk copy; stops an absurd request.
const MAX_DESTINATIONS: usize = 100;

#[derive(Debug, Deserialize)]
pub struct BulkDestination {
    pub facility_id: Uuid,
    pub target_task_id: String,
}

#[derive(Debug, Deserialize)]
pub struct BulkCopyRequest {
    pub source_facility_id: Option<Uuid>,
    /// The source task's name, kept on a background job so the page can say
    /// what it was copying.
    pub source_task_name: String,
    /// The source task, for the "Main tracker task - {task}" link every
    /// copied comment ends with.
    #[serde(default)]
    pub source_task_id: Option<String>,
    /// The comment as the person edited it; the same for every destination.
    pub comment: String,
    pub destinations: Vec<BulkDestination>,
    /// Also set each destination task to its list's complete status once its
    /// comment is posted. Off unless asked for.
    #[serde(default)]
    pub complete_tasks: bool,
    /// Run as a background job even when small. The page sets it when it
    /// copies several tasks together whose total is too much for one
    /// rate-limit window (each task is its own request).
    #[serde(default)]
    pub background: bool,
}

#[derive(Debug, Serialize)]
pub struct BulkCopyResponse {
    /// `inline`: done, `results` are in. `job`: running in the background;
    /// poll `job_id`.
    pub mode: &'static str,
    pub job_id: Option<Uuid>,
    pub total: usize,
    pub results: Vec<ItemResult>,
    pub copied: usize,
    pub failed: usize,
}

/// `POST /clients/{company_id}/clickup/bulk-copy`
pub async fn bulk_copy(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Json(request): Json<BulkCopyRequest>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    try_response!(
        user.require_permission(&state.db, PERMISSION, "clickup_bulk_copy", user_agent, None)
            .await
    );

    let comment = request.comment.trim().to_string();
    let mut seen = HashSet::new();
    if request.destinations.is_empty()
        || request.destinations.len() > MAX_DESTINATIONS
        || comment.is_empty()
        || comment.chars().count() > MAX_COMMENT_CHARS
        || request.source_task_name.trim().is_empty()
        || request.source_task_name.chars().count() > 300
        || request
            .source_task_id
            .as_deref()
            .is_some_and(|id| !valid_task_id(id.trim()))
        || request
            .destinations
            .iter()
            .any(|d| !valid_task_id(d.target_task_id.trim()) || !seen.insert(d.facility_id))
    {
        return bad_request(
            "invalid_copy_request",
            format!(
                "Choose between 1 and {MAX_DESTINATIONS} facilities (each once, with a task) and write a comment of up to 10,000 characters."
            ),
        );
    }

    let (ctx, token) = match tokio::try_join!(
        prepare_bulk(&state, &user, company_id, request.source_facility_id),
        load_user_token(&state, &user)
    ) {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    // Every destination must be a linked facility of this client other than
    // the source, and every task must be in that facility's own list.
    let lists: HashMap<Uuid, &ListInfo> = ctx
        .destinations
        .iter()
        .map(|list| (list.facility_id, list))
        .collect();
    let mut items = Vec::with_capacity(request.destinations.len());
    for destination in &request.destinations {
        let Some(list) = lists.get(&destination.facility_id) else {
            return bad_request(
                "invalid_destination",
                "A chosen facility is not a facility of this client with a ClickUp list linked."
                    .to_string(),
            );
        };
        let task_id = destination.target_task_id.trim();

        let tasks = match load_tasks_lenient(&state, &user, &token, list).await {
            Ok(tasks) => tasks,
            Err(message) => return conflict("clickup_list_unavailable", message),
        };
        if !tasks.iter().any(|task| task.id == task_id) {
            return bad_request(
                "task_not_in_linked_list",
                format!(
                    "A chosen task is not in {}'s ClickUp list (\"{}\").",
                    list.facility_name, list.list_name
                ),
            );
        }

        items.push(BulkItem {
            facility: (*list).clone(),
            target_task_id: task_id.to_string(),
            // The parent's own tasks need no pointer to the parent.
            pointer: ctx
                .parent
                .clone()
                .filter(|parent| parent.facility_id != list.facility_id),
        });
    }

    // The source task, for the footer link on every copied comment.
    let source_link = match request.source_task_id.as_deref().map(str::trim) {
        None => None,
        Some(id) => {
            let source_tasks = match load_tasks_lenient(&state, &user, &token, &ctx.source).await {
                Ok(tasks) => tasks,
                Err(message) => return conflict("clickup_list_unavailable", message),
            };
            match source_tasks.iter().find(|task| task.id == id) {
                Some(task) => Some(SourceLink::from_task(task)),
                None => {
                    return bad_request(
                        "task_not_in_source_list",
                        format!(
                            "The source task is not in {}'s ClickUp list (\"{}\").",
                            ctx.source.facility_name, ctx.source.list_name
                        ),
                    )
                }
            }
        }
    };

    let total = items.len();
    let source_facility_id = ctx.source.facility_id;

    let complete = request.complete_tasks;

    if !request.background
        && estimated_calls(total, ctx.parent.is_some(), complete) <= INLINE_CALL_BUDGET
    {
        let client = clickup_client(&state);
        let limiter = rate_limit::for_user(user.user_id);
        let mut results = Vec::with_capacity(total);
        for chunk in items.chunks(CHUNK_SIZE) {
            results.extend(
                copy_chunk(
                    &client,
                    &limiter,
                    &token,
                    &comment,
                    source_link.as_ref(),
                    complete,
                    chunk,
                )
                .await,
            );
        }
        finish_copy(
            &state,
            user.user_id,
            company_id,
            source_facility_id,
            &items,
            &results,
            complete,
            user_agent,
        )
        .await;

        let copied = results.iter().filter(|r| r.comment.ok).count();
        return Json(BulkCopyResponse {
            mode: "inline",
            job_id: None,
            total,
            copied,
            failed: total - copied,
            results,
        })
        .into_response();
    }

    // Too much for one request: run it in the background.
    let job_id = match jobs::create(
        &state.db,
        user.user_id,
        &user.role_keys,
        company_id,
        source_facility_id,
        request.source_task_name.trim(),
        total,
    )
    .await
    {
        Ok(id) => id,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "could not record a ClickUp Copy job");
            return internal_error("Could not start the copy");
        }
    };

    tokio::spawn(run_job(
        state.clone(),
        user.user_id,
        user.role_keys.clone(),
        token,
        comment,
        source_link,
        complete,
        items,
        job_id,
        company_id,
        source_facility_id,
        user_agent.map(str::to_string),
    ));

    (
        StatusCode::ACCEPTED,
        Json(BulkCopyResponse {
            mode: "job",
            job_id: Some(job_id),
            total,
            results: Vec::new(),
            copied: 0,
            failed: 0,
        }),
    )
        .into_response()
}
