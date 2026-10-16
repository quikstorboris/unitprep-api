//! `bulk-comment`: the source task's latest comment, to prefill the box.

use axum::{
    extract::{Json, Path, Query, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::super::comments::SourceComment;
use super::super::lists::{load_tasks_lenient, valid_task_id, PERMISSION};
use super::context::prepare_bulk;
use crate::api::clickup_connection::{clickup_client, load_user_token};
use crate::api::rls::try_response;
use crate::api::{bad_request, conflict, AppState};
use crate::auth::AuthenticatedUser;
use crate::clickup::comments::latest;
use crate::clickup::rate_limit;

#[derive(Debug, Deserialize)]
pub struct BulkCommentQuery {
    pub source_facility_id: Option<Uuid>,
    pub source_task_id: String,
}

#[derive(Debug, Serialize)]
pub struct BulkCommentResponse {
    /// The source task's latest comment: what the box is prefilled with.
    pub source_comment: Option<SourceComment>,
}

/// `GET /clients/{company_id}/clickup/bulk-comment`
pub async fn bulk_comment(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
    Query(query): Query<BulkCommentQuery>,
) -> Response {
    try_response!(
        user.require_permission(&state.db, PERMISSION, "clickup_bulk_comment", None, None)
            .await
    );
    let source_task_id = query.source_task_id.trim().to_string();
    if !valid_task_id(&source_task_id) {
        return bad_request("invalid_clickup_task", "Choose a ClickUp task.".to_string());
    }

    let (ctx, token) = match tokio::try_join!(
        prepare_bulk(&state, &user, company_id, query.source_facility_id),
        load_user_token(&state, &user)
    ) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let source_tasks = match load_tasks_lenient(&state, &user, &token, &ctx.source).await {
        Ok(tasks) => tasks,
        Err(message) => return conflict("clickup_list_unavailable", message),
    };
    if !source_tasks.iter().any(|task| task.id == source_task_id) {
        return bad_request(
            "task_not_in_list",
            "That task is not in the source facility's ClickUp list.".to_string(),
        );
    }

    rate_limit::for_user(user.user_id).acquire().await;
    let comments = match clickup_client(&state)
        .task_comments(&token, &source_task_id)
        .await
    {
        Ok(comments) => comments,
        Err(err) => return super::super::comments::comment_reads(&err),
    };

    Json(BulkCommentResponse {
        source_comment: latest(&comments).map(|comment| SourceComment {
            text: comment.text.clone(),
            author: comment.author.clone(),
            date_ms: comment.date_ms,
        }),
    })
    .into_response()
}
