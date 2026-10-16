//! Posting the run's results to the confirmed task: comment, assignee,
//! complete.

use axum::{
    extract::{Json, Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use uuid::Uuid;

use super::link::results_link;
use super::prepare::prepare;
use super::{PostResultsRequest, PostResultsResponse, StepOutcome, PERMISSION};
use crate::api::clickup_connection::{
    clickup_client, clickup_failure_response, load_user_credentials,
};
use crate::api::rls::try_response;
use crate::api::{bad_request, conflict, not_found, user_agent_from, AppState};
use crate::auth::AuthenticatedUser;
use crate::clickup::task_cache;
use crate::clickup::tasks::{completion_status, ClickUpTask};
use crate::clickup::ClickUpError;
use crate::client_ops::{audit_log, tool_runs as run_store};

/// Runs `work` and logs how long it took, so a slow update names the call
/// that was slow (ClickUp, Dropbox or the database) instead of one total.
async fn timed<T>(step: &'static str, work: impl std::future::Future<Output = T>) -> T {
    let started = std::time::Instant::now();
    let result = work.await;
    tracing::info!(
        step,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "ClickUp run-update step"
    );
    result
}

fn failure_message(err: &ClickUpError) -> String {
    match err {
        ClickUpError::Unauthorized => "ClickUp rejected your token.".to_string(),
        ClickUpError::NotFound => "ClickUp no longer shows this task.".to_string(),
        other => format!("{other}"),
    }
}

/// Posts the run's results to the confirmed task: comment, assignee,
/// complete. The three writes are separate ClickUp calls and cannot be
/// made atomic, so each one's outcome is reported on its own and a
/// failure of one does not stop the others.
pub async fn post_run_update(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<PostResultsRequest>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    try_response!(
        user.require_permission(
            &state.db,
            PERMISSION,
            "post_clickup_run_update",
            user_agent,
            None,
        )
        .await
    );

    let task_id = request.task_id.trim();
    if task_id.is_empty()
        || task_id.len() > 64
        || !task_id.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return bad_request("invalid_clickup_task", "Choose a ClickUp task.".to_string());
    }

    let (prepared, (token, clickup_user_id)) = match tokio::try_join!(
        prepare(
            &state,
            &user,
            company_id,
            facility_id,
            request.session_id.trim()
        ),
        timed("credentials", load_user_credentials(&state, &user))
    ) {
        Ok(pair) => pair,
        Err(response) => return response,
    };
    let Some(clickup_user) = clickup_user_id.and_then(|id| id.parse::<i64>().ok()) else {
        return conflict(
            "clickup_not_connected",
            "Reconnect your ClickUp account (My Integrations > ClickUp) so Orchestrator knows who you are in ClickUp."
                .to_string(),
        );
    };
    let client = clickup_client(&state);

    // Later runs only comment; the others also need the
    // list's complete status. Everything read here is independent (the
    // task, the list's statuses, the Dropbox link), so it is read at once.
    let comment_only = prepared.comment_only();
    let (task, statuses, link) = tokio::join!(
        timed("read_task", client.task(&token, task_id)),
        async {
            if comment_only {
                None
            } else {
                Some(
                    timed(
                        "read_statuses",
                        client.list_statuses(&token, &prepared.list_id),
                    )
                    .await,
                )
            }
        },
        async {
            match (&prepared.output_path, &prepared.output_link) {
                // Captured when the file was saved: no Dropbox call.
                (Some(_), Some(link)) => Some((link.clone(), "shared")),
                (Some(path), None) => {
                    let made = timed("dropbox_link", results_link(&state, path)).await;
                    if made.1 == "shared" {
                        // Keep it for next time (and for a re-post).
                        let (db, actor, roles) =
                            (state.db.clone(), user.user_id, user.role_keys.clone());
                        let (session_id, path, link) =
                            (prepared.session_id.clone(), path.clone(), made.0.clone());
                        tokio::spawn(async move {
                            run_store::store_output_dropbox_link(
                                &db,
                                actor,
                                &roles,
                                &session_id,
                                &path,
                                &link,
                            )
                            .await
                        });
                    }
                    Some(made)
                }
                (None, _) => None,
            }
        }
    );

    // Checks before any write: the task is really in this facility's
    // list, and the list has a status that means complete. Nothing has
    // changed in ClickUp if either fails.
    let task: ClickUpTask = match task {
        Ok(task) => task,
        Err(ClickUpError::NotFound) => {
            return not_found(
                "clickup_task_not_found",
                "ClickUp no longer shows that task.".to_string(),
            )
        }
        Err(err) => return clickup_failure_response(&state, &user, &err).await,
    };

    if task.list_id.as_deref() != Some(prepared.list_id.as_str()) {
        return bad_request(
            "task_not_in_linked_list",
            format!(
                "That task is not in this facility's ClickUp list (\"{}\").",
                prepared.list_name
            ),
        );
    }

    let complete_status = match statuses {
        None => None,
        Some(Err(err)) => return clickup_failure_response(&state, &user, &err).await,
        Some(Ok(statuses)) => match completion_status(&statuses) {
            Some(status) => Some(status.name.clone()),
            None => {
                return conflict(
                    "clickup_no_complete_status",
                    format!(
                        "The ClickUp list \"{}\" has no complete status, so nothing was changed.",
                        prepared.list_name
                    ),
                )
            }
        },
    };

    let link_kind = link.as_ref().map_or("none", |(_, kind)| kind);
    let parts: Vec<(&str, Option<&str>)> = match &link {
        Some((url, _)) => vec![
            (prepared.comment.lead.as_str(), None),
            (prepared.comment.link_text.as_str(), Some(url.as_str())),
        ],
        None => vec![(prepared.comment.without_link.as_str(), None)],
    };

    // The three writes touch different things on the task, so they are
    // sent together; each reports its own outcome.
    let (comment, assignee, status) = tokio::join!(
        timed("write_comment", client.add_comment(&token, task_id, &parts)),
        async {
            match &complete_status {
                Some(_) => Some(
                    timed(
                        "write_assignee",
                        client.add_assignee(&token, task_id, clickup_user),
                    )
                    .await,
                ),
                None => None,
            }
        },
        async {
            match &complete_status {
                Some(name) => {
                    Some(timed("write_status", client.set_status(&token, task_id, name)).await)
                }
                None => None,
            }
        }
    );

    // A written task is stale in the cached list.
    task_cache::invalidate_list(&prepared.list_id);

    if matches!(comment, Err(ClickUpError::Unauthorized)) {
        return clickup_failure_response(&state, &user, &ClickUpError::Unauthorized).await;
    }
    let outcome = |result: Result<(), ClickUpError>| match result {
        Ok(()) => StepOutcome::done(),
        Err(err) => StepOutcome::failed(failure_message(&err)),
    };
    let comment = outcome(comment);
    let assignee = assignee.map(outcome);
    let status = status.map(outcome);

    // Duplicate checks keep their original event type so older log rows and
    // new ones read as one history.
    let event = if prepared.tool == "dedup" {
        audit_log::event::FACILITY_CLICKUP_DUPLICATE_CHECK_POSTED
    } else {
        audit_log::event::FACILITY_CLICKUP_RUN_POSTED
    };
    audit_log::record(
        &state.db,
        event,
        user.user_id,
        "facility",
        Some(&facility_id.to_string()),
        audit_log::Change::none(),
        user_agent,
        None,
        serde_json::json!({
            "company_id": company_id,
            "session_id": request.session_id,
            "tool": prepared.tool,
            "step": prepared.step.step_key,
            "clickup_task_id": task.id,
            "clickup_task_name": task.name,
            "sequence_number": prepared.sequence_number,
            "link_kind": link_kind,
            "comment_ok": comment.ok,
            "assignee_ok": assignee.as_ref().map(|o| o.ok),
            "status_ok": status.as_ref().map(|o| o.ok),
        }),
    )
    .await;

    Json(PostResultsResponse {
        task_name: task.name.trim().to_string(),
        task_url: task.url,
        link_kind,
        comment,
        assignee,
        status,
    })
    .into_response()
}
