//! `GET .../clickup/copy-comments`: one row's source comment (the prefill)
//! and whether the target looks as if it already has it. Fetched per row
//! so the dialog does not read every task's comments up front.

use axum::{
    extract::{Json, Path, Query, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::lists::{load_tasks, prepare, valid_task_id, PERMISSION};
use crate::api::clickup_connection::{clickup_client, load_user_token};
use crate::api::{bad_request, internal_error, not_found, AppState};
use crate::auth::AuthenticatedUser;
use crate::clickup::comments::latest;
use crate::clickup::copy_text;
use crate::clickup::ClickUpError;

#[derive(Debug, Deserialize)]
pub struct CommentsQuery {
    pub source_task_id: String,
    pub target_task_id: String,
    pub source_facility_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct SourceComment {
    pub text: String,
    pub author: String,
    pub date_ms: i64,
}

#[derive(Debug, Serialize)]
pub struct CommentsResponse {
    /// The source task's latest comment: what the row is prefilled with.
    pub source_comment: Option<SourceComment>,
    /// The target task already has a comment that reads the same.
    pub already_copied: bool,
    /// The target task already has the main-list pointer comment.
    pub pointer_present: bool,
}

pub(super) fn comment_reads(err: &ClickUpError) -> Response {
    match err {
        ClickUpError::NotFound => not_found(
            "clickup_task_not_found",
            "ClickUp no longer shows that task (or you can't see it).".to_string(),
        ),
        other => internal_error(&format!("Could not read the task's comments: {other}")),
    }
}

/// `GET .../clickup/copy-comments`
pub async fn copy_comments(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<CommentsQuery>,
) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "clickup_copy_comments", None, None)
        .await
    {
        return response;
    }
    let (source_task, target_task) = (query.source_task_id.trim(), query.target_task_id.trim());
    if !valid_task_id(source_task) || !valid_task_id(target_task) {
        return bad_request("invalid_clickup_task", "Choose a ClickUp task.".to_string());
    }

    let (prepared, token) = match tokio::try_join!(
        prepare(
            &state,
            &user,
            company_id,
            facility_id,
            query.source_facility_id
        ),
        load_user_token(&state, &user)
    ) {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    // Both tasks must be in the lists this request is about (the lists
    // are cached, so this is normally free).
    let (source_tasks, target_tasks) = match tokio::try_join!(
        load_tasks(&state, &user, &token, &prepared.source),
        load_tasks(&state, &user, &token, &prepared.target)
    ) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    if !source_tasks.iter().any(|task| task.id == source_task)
        || !target_tasks.iter().any(|task| task.id == target_task)
    {
        return bad_request(
            "task_not_in_list",
            "That task is not in the facility's ClickUp list.".to_string(),
        );
    }

    let client = clickup_client(&state);
    let (source_comments, target_comments) = tokio::join!(
        client.task_comments(&token, source_task),
        client.task_comments(&token, target_task)
    );
    let (source_comments, target_comments) = match (source_comments, target_comments) {
        (Ok(source), Ok(target)) => (source, target),
        (Err(err), _) | (_, Err(err)) => return comment_reads(&err),
    };

    let source_comment = latest(&source_comments);
    Json(CommentsResponse {
        already_copied: source_comment.is_some_and(|source| {
            target_comments
                .iter()
                .any(|existing| copy_text::looks_already_copied(&existing.text, &source.text))
        }),
        pointer_present: target_comments
            .iter()
            .any(|existing| copy_text::is_pointer_comment(&existing.text)),
        source_comment: source_comment.map(|comment| SourceComment {
            text: comment.text.clone(),
            author: comment.author.clone(),
            date_ms: comment.date_ms,
        }),
    })
    .into_response()
}
