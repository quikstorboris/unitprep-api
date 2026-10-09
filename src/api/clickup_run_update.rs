//! Posting a finished tool run to the facility's ClickUp task: once the
//! run's results file is saved to the facility's Dropbox folder,
//! Orchestrator looks in the facility's linked ClickUp list for the task
//! that stands for this run, lets the person confirm which one, then
//! comments (the wording's last words, *here*, link the saved file), adds
//! the person as an assignee and sets the task to its list's complete
//! status.
//!
//! Which task a run stands for, and what the comment says, are rows of
//! `integrations.clickup_task_steps` (matched by the run's tool and its
//! position among the facility's runs of that tool: a duplicate check has
//! a 1st and a 2nd step, Unit Groups and the Template Tagger one each),
//! never Rust constants -- so a renamed task or new wording is a data
//! edit, and a new tool is a row plus the tool in `tool_runs`.
//!
//! Everything about the run is read from the database, never taken from
//! the browser: which step this is (the run's position among the
//! facility's runs of its tool), where its file was saved, and which list
//! the facility is linked to. The one thing the browser names is the task,
//! and that is re-read from ClickUp and refused unless it really lives
//! in the facility's linked list.
//!
//! All calls use the caller's own ClickUp token, so ClickUp's activity
//! history shows who did it.

use axum::{
    extract::{Json, Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::clickup_connection::{
    clickup_client, clickup_failure_response, load_user_credentials, load_user_token,
};
use crate::api::rls::try_response;
use crate::api::tool_runs::facility_belongs_to_company;
use crate::api::{bad_request, conflict, internal_error, not_found, user_agent_from, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clickup::task_cache;
use crate::clickup::task_matching::{self, StepDefinition};
use crate::clickup::tasks::{completion_status, ClickUpTask};
use crate::clickup::ClickUpError;
use crate::client_ops::{audit_log, tool_runs as run_store};

const PERMISSION: &str = "integrations.clickup";

/// Everything the two endpoints need, read from the database.
struct Prepared {
    list_id: String,
    list_name: String,
    list_url: String,
    /// The run's own session id, whichever of its two ids the request used.
    session_id: String,
    sequence_number: i64,
    /// Where the summary file was saved in Dropbox, if it was.
    output_path: Option<String>,
    /// The share link captured when the file was saved, if it was ready.
    output_link: Option<String>,
    step: StepDefinition,
    /// The step's comment wording: the lead-in, the linked word, and the
    /// whole comment used when there is no file to link (the results were
    /// only downloaded, or nothing was saved yet -- the person adds the
    /// file to the task by hand).
    comment: CommentWording,
    /// The run position from which an update only adds a comment: the
    /// task is already assigned and complete.
    comment_only_from_sequence: i64,
    /// The run's tool (`dedup`, `unit_group`, `tagger`).
    tool: String,
}

struct CommentWording {
    lead: String,
    link_text: String,
    without_link: String,
}

impl Prepared {
    fn comment_only(&self) -> bool {
        self.sequence_number >= self.comment_only_from_sequence
    }
}

#[derive(Debug, Deserialize)]
pub struct CandidatesQuery {
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct TaskCandidate {
    pub task_id: String,
    pub name: String,
    /// The task's parent, when it is a subtask -- the same step name
    /// repeats under several parents in a real list.
    pub parent_name: Option<String>,
    pub status: String,
    pub is_finished: bool,
    pub assignees: Vec<String>,
    pub url: String,
    pub score: f64,
}

#[derive(Debug, Serialize)]
pub struct CandidatesResponse {
    /// "1st Duplicate Check", "2nd Duplicate Check", "Unit Groups", ...
    pub step_label: String,
    pub sequence_number: i64,
    pub list_name: String,
    pub list_url: String,
    /// Whether the summary file is saved in Dropbox, so the comment can
    /// link it. When false the comment has no link and the person adds
    /// the file by hand.
    pub file_link_available: bool,
    /// Later runs only add a comment (no assignee, no status).
    pub comment_only: bool,
    pub candidates: Vec<TaskCandidate>,
}

#[derive(Debug, Deserialize)]
pub struct PostResultsRequest {
    pub session_id: String,
    pub task_id: String,
}

/// How one of the three ClickUp writes went.
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct StepOutcome {
    pub ok: bool,
    pub message: Option<String>,
}

impl StepOutcome {
    fn done() -> Self {
        Self {
            ok: true,
            message: None,
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: Some(message.into()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PostResultsResponse {
    pub task_name: String,
    pub task_url: String,
    /// What the comment links: `shared` (a Dropbox share link), `path`
    /// (Dropbox would not make one, so the file's plain Dropbox web path,
    /// which opens for anyone with the folder) or `none` (no Dropbox
    /// file; the person adds it by hand).
    pub link_kind: &'static str,
    pub comment: StepOutcome,
    /// `None` when not part of this update (later runs).
    pub assignee: Option<StepOutcome>,
    pub status: Option<StepOutcome>,
}

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

fn server_error(context: &'static str, err: &sqlx::Error, user: &AuthenticatedUser) -> Response {
    tracing::error!(error = %err, user_id = %user.user_id, "{context}");
    internal_error("Could not look up this run for ClickUp")
}

/// Reads the facility's linked list, the run and the step definition, or
/// the response to return instead.
async fn prepare(
    state: &AppState,
    user: &AuthenticatedUser,
    company_id: Uuid,
    facility_id: Uuid,
    session_id: &str,
) -> Result<Prepared, Response> {
    let fail = |err: sqlx::Error| server_error("ClickUp run lookup failed", &err, user);

    let mut tx = begin_rls_transaction(&state.db, user.user_id, &user.role_keys)
        .await
        .map_err(fail)?;

    if !facility_belongs_to_company(&mut tx, facility_id, company_id)
        .await
        .map_err(fail)?
    {
        let _ = tx.commit().await;
        return Err(not_found("not_found", "No such facility.".to_string()));
    }

    let link: (Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT clickup_list_id, clickup_list_name, clickup_list_url
           FROM clients.facilities WHERE id = $1",
    )
    .bind(facility_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(fail)?;

    // `session_id` may be the run's own session id (right after a check) or
    // the run's row id (what the Onboarding Work tab lists), so a check can
    // be posted later from either place.
    let run: Option<RunRow> = sqlx::query_as(
        "SELECT tool, sequence_number, session_id, output_dropbox_path, output_dropbox_link FROM (
             SELECT id, tool, session_id, output_dropbox_path, output_dropbox_link,
                    ROW_NUMBER() OVER (PARTITION BY facility_id, tool ORDER BY created_at ASC) AS sequence_number
               FROM client_ops.tool_runs
              WHERE facility_id = $1
         ) runs WHERE session_id = $2 OR id::text = $2",
    )
    .bind(facility_id)
    .bind(session_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(fail)?;

    // The run's step is the tool's step with the highest ordinal not above
    // the run's position (a 3rd duplicate check uses the 2nd check's step).
    let step: Option<StepRow> = match &run {
        Some((tool, sequence_number, ..)) => sqlx::query_as(
            "SELECT step_key, label, ordinal, phrases, comment_lead, comment_link_text,
                    comment_without_link, comment_only_from_sequence
               FROM integrations.clickup_task_steps
              WHERE tool = $1 AND ordinal <= $2
              ORDER BY ordinal DESC LIMIT 1",
        )
        .bind(tool)
        .bind(*sequence_number as i32)
        .fetch_optional(&mut *tx)
        .await
        .map_err(fail)?,
        None => None,
    };

    tx.commit().await.map_err(fail)?;

    let (Some(list_id), Some(list_name), Some(list_url)) = link else {
        return Err(conflict(
            "facility_not_linked_to_clickup",
            "Link this facility to its ClickUp list first (the ClickUp section on its General tab)."
                .to_string(),
        ));
    };

    let Some((tool, sequence_number, session_id, output_path, output_link)) = run else {
        return Err(not_found(
            "not_found",
            "No run with that session was recorded for this facility.".to_string(),
        ));
    };

    let Some((
        step_key,
        label,
        ordinal,
        phrases,
        comment_lead,
        comment_link_text,
        comment_without_link,
        comment_only_from_sequence,
    )) = step
    else {
        tracing::warn!(tool, "no ClickUp task step is configured for this tool");
        return Err(conflict(
            "clickup_step_not_configured",
            "No ClickUp task is configured for this kind of run yet.".to_string(),
        ));
    };

    Ok(Prepared {
        list_id,
        list_name,
        list_url,
        session_id,
        sequence_number,
        output_path,
        output_link,
        step: StepDefinition {
            step_key,
            label,
            ordinal,
            phrases,
        },
        comment: CommentWording {
            lead: comment_lead,
            link_text: comment_link_text,
            without_link: comment_without_link,
        },
        comment_only_from_sequence: i64::from(comment_only_from_sequence),
        tool,
    })
}

/// One `integrations.clickup_task_steps` row: key, label, ordinal, task-name
/// phrases, comment lead-in, linked word, no-link comment, and the run
/// position from which only a comment is added.
/// One tool run: tool, position among the facility's runs of it, session id,
/// saved Dropbox path and its captured share link.
type RunRow = (String, i64, String, Option<String>, Option<String>);

type StepRow = (
    String,
    String,
    i32,
    Vec<String>,
    String,
    String,
    String,
    i32,
);

/// Percent-encodes everything but unreserved characters (RFC 3986), for
/// a Dropbox web path.
fn encode_component(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// The file's plain Dropbox web location -- the same `/home/<path>` form
/// the app's "Open Destination Folder" button uses, opened on the file
/// with `preview=`. Used when Dropbox will not create a share link.
fn dropbox_web_url(path: &str) -> String {
    let (folder, file) = path.rsplit_once('/').unwrap_or(("", path));
    let folder = folder
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(encode_component)
        .collect::<Vec<_>>()
        .join("/");

    format!(
        "https://www.dropbox.com/home/{folder}?preview={}",
        encode_component(file)
    )
}

/// A Dropbox link for the saved file and its kind: `shared` for a real
/// share link, `path` for the plain web path used when Dropbox will not
/// create one.
async fn results_link(state: &AppState, path: &str) -> (String, &'static str) {
    match state.dropbox.shared_link(path).await {
        Ok(url) => (url, "shared"),
        Err(err) => {
            tracing::warn!(error = %err, path, "Dropbox share link unavailable; using the plain web path");
            (dropbox_web_url(path), "path")
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_web_path_fallback_opens_the_file_in_its_folder() {
        assert_eq!(
            dropbox_web_url(
                "/QMS Onboarding/Acme & Sons/Duplicate Check/ACME_v1_pull_check_10-05-2026.xlsx"
            ),
            "https://www.dropbox.com/home/QMS%20Onboarding/Acme%20%26%20Sons/Duplicate%20Check?preview=ACME_v1_pull_check_10-05-2026.xlsx"
        );
    }

    #[test]
    fn a_path_with_no_folder_still_makes_a_link() {
        assert_eq!(
            dropbox_web_url("file.xlsx"),
            "https://www.dropbox.com/home/?preview=file.xlsx"
        );
    }
}
