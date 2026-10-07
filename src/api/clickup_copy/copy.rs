//! `POST .../clickup/copy`: posts the (edited) comments the facility
//! dialog collected onto the facility's own tasks, plus -- once per target
//! task -- the pointer comment naming the company's main list.
//!
//! The dialog confirms one row at a time, so a request is capped at
//! [`MAX_ITEMS`]; copying one comment to many facilities is the client's
//! bulk copy (`bulk`), which can run as a background job.

use axum::{
    extract::{Json, Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::exec::{copy_one, ItemResult, SourceLink};
use super::lists::{load_tasks, prepare, valid_task_id, MAX_COMMENT_CHARS, PERMISSION};
use crate::api::clickup_connection::{clickup_client, load_user_token};
use crate::api::{bad_request, user_agent_from, AppState};
use crate::auth::AuthenticatedUser;
use crate::clickup::rate_limit;
use crate::clickup::task_cache;
use crate::client_ops::audit_log;

/// Rows per request from the dialog.
const MAX_ITEMS: usize = 30;

/// Rows written at once.
const WRITE_CONCURRENCY: usize = 4;

#[derive(Debug, Deserialize)]
pub struct CopyItem {
    /// The target task the comment is posted on.
    pub target_task_id: String,
    /// The comment as the person edited it.
    pub comment: String,
    /// The source task the comment came from. When given, the comment
    /// ends with a "Main tracker task - {task}" link to it.
    #[serde(default)]
    pub source_task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CopyRequest {
    pub source_facility_id: Option<Uuid>,
    pub items: Vec<CopyItem>,
    /// Also set each target task to its list's complete status once its
    /// comment is posted. Off unless asked for: copying only comments is
    /// the normal case.
    #[serde(default)]
    pub complete_tasks: bool,
}

#[derive(Debug, Serialize)]
pub struct CopyResponse {
    pub results: Vec<ItemResult>,
    pub copied: usize,
    pub failed: usize,
}

/// `POST .../clickup/copy`
pub async fn copy_comments_to_tasks(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<CopyRequest>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    if let Err(response) = user
        .require_permission(
            &state.db,
            PERMISSION,
            "clickup_copy_comments",
            user_agent,
            None,
        )
        .await
    {
        return response;
    }

    if request.items.is_empty() || request.items.len() > MAX_ITEMS {
        return bad_request(
            "invalid_copy_request",
            format!("Copy between 1 and {MAX_ITEMS} comments at a time."),
        );
    }
    for item in &request.items {
        let comment = item.comment.trim();
        if !valid_task_id(item.target_task_id.trim())
            || item
                .source_task_id
                .as_deref()
                .is_some_and(|id| !valid_task_id(id.trim()))
            || comment.is_empty()
            || comment.chars().count() > MAX_COMMENT_CHARS
        {
            return bad_request(
                "invalid_copy_request",
                "Every row needs a target task and a comment of up to 10,000 characters."
                    .to_string(),
            );
        }
    }

    let (prepared, token) = match tokio::try_join!(
        prepare(
            &state,
            &user,
            company_id,
            facility_id,
            request.source_facility_id
        ),
        load_user_token(&state, &user)
    ) {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    // Writes go only to tasks in the facility's own list.
    let target_tasks = match load_tasks(&state, &user, &token, &prepared.target).await {
        Ok(tasks) => tasks,
        Err(response) => return response,
    };
    if request.items.iter().any(|item| {
        let id = item.target_task_id.trim();
        !target_tasks.iter().any(|task| task.id == id)
    }) {
        return bad_request(
            "task_not_in_linked_list",
            format!(
                "A chosen task is not in this facility's ClickUp list (\"{}\").",
                prepared.target.list_name
            ),
        );
    }

    // The source task each comment came from, for its footer link. Read
    // from the source facility's list, so the link is ClickUp's own.
    let mut links: Vec<Option<SourceLink>> = Vec::with_capacity(request.items.len());
    let source_tasks = if request
        .items
        .iter()
        .any(|item| item.source_task_id.is_some())
    {
        match load_tasks(&state, &user, &token, &prepared.source).await {
            Ok(tasks) => Some(tasks),
            Err(response) => return response,
        }
    } else {
        None
    };
    for item in &request.items {
        links.push(match (&item.source_task_id, &source_tasks) {
            (Some(id), Some(tasks)) => match tasks.iter().find(|task| task.id == id.trim()) {
                Some(task) => Some(SourceLink::from_task(task)),
                None => {
                    return bad_request(
                        "task_not_in_source_list",
                        format!(
                            "A source task is not in {}'s ClickUp list (\"{}\").",
                            prepared.source.facility_name, prepared.source.list_name
                        ),
                    )
                }
            },
            _ => None,
        });
    }

    // The parent's own tasks need no pointer to the parent.
    let pointer_list = prepared
        .parent
        .as_ref()
        .filter(|parent| parent.facility_id != prepared.target.facility_id);

    let client = clickup_client(&state);
    let limiter = rate_limit::for_user(user.user_id);

    // A few rows at a time, in the order given.
    let mut results: Vec<ItemResult> = Vec::with_capacity(request.items.len());
    for (chunk, link_chunk) in request
        .items
        .chunks(WRITE_CONCURRENCY)
        .zip(links.chunks(WRITE_CONCURRENCY))
    {
        results.extend(
            futures::future::join_all(chunk.iter().zip(link_chunk).map(|(item, link)| {
                copy_one(
                    &client,
                    &limiter,
                    &token,
                    item.target_task_id.trim(),
                    &item.comment,
                    link.as_ref(),
                    pointer_list,
                    request
                        .complete_tasks
                        .then_some(prepared.target.list_id.as_str()),
                )
            }))
            .await,
        );
    }

    // The list changed under the cached copy.
    task_cache::invalidate_list(&prepared.target.list_id);

    let copied = results.iter().filter(|result| result.comment.ok).count();
    let failed = results.len() - copied;

    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_CLICKUP_COMMENTS_COPIED,
        user.user_id,
        "facility",
        Some(&facility_id.to_string()),
        audit_log::Change::none(),
        user_agent,
        None,
        serde_json::json!({
            "company_id": company_id,
            "source_facility_id": prepared.source.facility_id,
            "copied": copied,
            "failed": failed,
            "pointers_posted": results.iter().filter(|r| r.pointer.state == "posted").count(),
            "complete_requested": request.complete_tasks,
            "tasks_completed": results
                .iter()
                .filter(|r| r.completed.as_ref().is_some_and(|outcome| outcome.ok))
                .count(),
        }),
    )
    .await;

    Json(CopyResponse {
        results,
        copied,
        failed,
    })
    .into_response()
}
