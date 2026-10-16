//! The tasks that could be a run's, for the person to confirm.

use axum::{
    extract::{Json, Path, Query, State},
    response::{IntoResponse, Response},
};
use uuid::Uuid;

use super::prepare::prepare;
use super::{CandidatesQuery, CandidatesResponse, TaskCandidate, PERMISSION};
use crate::api::clickup_connection::{clickup_client, clickup_failure_response, load_user_token};
use crate::api::rls::try_response;
use crate::api::{not_found, AppState};
use crate::auth::AuthenticatedUser;
use crate::clickup::task_cache;
use crate::clickup::task_matching;
use crate::clickup::ClickUpError;

/// The tasks in the facility's linked list that could be this run's,
/// best match first. Always a list, even with one entry: the person
/// confirms the task before anything is written.
pub async fn run_update_tasks(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<CandidatesQuery>,
) -> Response {
    try_response!(
        user.require_permission(
            &state.db,
            PERMISSION,
            "clickup_run_update_tasks",
            None,
            None,
        )
        .await
    );

    // The database lookups and the token read are independent: one wait.
    let (prepared, token) = match tokio::try_join!(
        prepare(
            &state,
            &user,
            company_id,
            facility_id,
            query.session_id.trim()
        ),
        load_user_token(&state, &user)
    ) {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    // A list's tasks are a big read, and the panel asks again whenever it
    // reopens, so a minute-old copy is reused.
    let tasks = match task_cache::get_or_load(user.user_id, &prepared.list_id, || async {
        clickup_client(&state)
            .list_tasks(&token, &prepared.list_id)
            .await
    })
    .await
    {
        Ok(tasks) => tasks,
        Err(ClickUpError::NotFound) => {
            return not_found(
                "clickup_list_not_found",
                format!(
                    "ClickUp no longer shows the linked list \"{}\". Re-link this facility.",
                    prepared.list_name
                ),
            )
        }
        Err(err) => return clickup_failure_response(&state, &user, &err).await,
    };

    let parents = task_matching::parent_names(&tasks);
    let candidates = task_matching::rank(&prepared.step, &tasks)
        .into_iter()
        .map(|ranked| TaskCandidate {
            task_id: ranked.task.id.clone(),
            name: ranked.task.name.trim().to_string(),
            parent_name: ranked
                .task
                .parent_id
                .as_deref()
                .and_then(|id| parents.get(id))
                .map(|name| name.trim().to_string()),
            status: ranked.task.status.clone(),
            is_finished: ranked.task.is_finished(),
            assignees: ranked
                .task
                .assignees
                .iter()
                .map(|a| a.username.clone())
                .collect(),
            url: ranked.task.url.clone(),
            score: (ranked.score * 100.0).round() / 100.0,
        })
        .collect();

    Json(CandidatesResponse {
        file_link_available: prepared.output_path.is_some(),
        comment_only: prepared.comment_only(),
        step_label: prepared.step.label,
        sequence_number: prepared.sequence_number,
        list_name: prepared.list_name,
        list_url: prepared.list_url,
        candidates,
    })
    .into_response()
}
