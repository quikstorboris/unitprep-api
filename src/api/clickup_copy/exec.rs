//! Posting one copied comment to its target task, and the once-per-task
//! "main task list" pointer after it. Shared by the facility dialog's
//! copy, the client's bulk copy and the background jobs that run a bulk
//! copy too big to do inside one request.
//!
//! Every ClickUp call first takes a slot from the user's rate limiter
//! (`clickup::rate_limit`), so any number of rows -- from any of those
//! paths at once -- stays under ClickUp's per-token limit instead of
//! failing rows with 429s.

use serde::Serialize;

use super::lists::ListInfo;
use crate::clickup::comments::ClickUpComment;
use crate::clickup::copy_text;
use crate::clickup::rate_limit::RateLimiter;
use crate::clickup::tasks::{completion_status, ClickUpTask};
use crate::clickup::{ClickUpClient, ClickUpError};

/// A bulk copy whose ClickUp calls fit in this many runs inside the
/// request that asked for it; a bigger one becomes a background job the
/// person is notified about. (About a minute of work at the rate limit.)
pub(super) const INLINE_CALL_BUDGET: usize = 50;

/// ClickUp calls one row costs: its comment; when a pointer may be needed,
/// reading the task's comments to see whether it is already there, then
/// posting it; and, when the task is to be completed, reading the list's
/// statuses and setting the status. The estimate is the worst case.
pub(super) fn calls_per_row(pointer_possible: bool, complete: bool) -> usize {
    1 + if pointer_possible { 2 } else { 0 } + if complete { 2 } else { 0 }
}

pub(super) fn estimated_calls(rows: usize, pointer_possible: bool, complete: bool) -> usize {
    rows * calls_per_row(pointer_possible, complete)
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub ok: bool,
    pub message: Option<String>,
}

impl Outcome {
    fn done() -> Self {
        Self {
            ok: true,
            message: None,
        }
    }

    pub(super) fn failed(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: Some(message.into()),
        }
    }
}

/// The task a comment was copied from: its name and where to open it, for
/// the "Main tracker task - ..." footer on every copied comment.
#[derive(Debug, Clone)]
pub(super) struct SourceLink {
    pub name: String,
    pub url: String,
}

impl SourceLink {
    pub(super) fn from_task(task: &ClickUpTask) -> Self {
        Self {
            name: task.name.trim().to_string(),
            // The list read normally carries the task's URL; ClickUp's own
            // short form opens the same task if it ever does not.
            url: if task.url.trim().is_empty() {
                format!("https://app.clickup.com/t/{}", task.id)
            } else {
                task.url.clone()
            },
        }
    }
}

/// What happened to the once-per-task pointer comment.
#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct PointerOutcome {
    /// `posted`, `already_present`, `not_applicable` (no parent
    /// designated, or this is the parent's own task) or `failed`.
    pub state: &'static str,
    pub message: Option<String>,
}

impl PointerOutcome {
    pub(super) fn state(state: &'static str) -> Self {
        Self {
            state,
            message: None,
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Self {
            state: "failed",
            message: Some(message.into()),
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct ItemResult {
    pub target_task_id: String,
    pub comment: Outcome,
    pub pointer: PointerOutcome,
    /// Whether the task was set to its list's complete status. Only
    /// present when completing was asked for and the comment went through.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<Outcome>,
    /// Which facility's task this was, for a copy to several facilities.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facility_id: Option<uuid::Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facility_name: Option<String>,
}

pub(super) fn failure_message(err: &ClickUpError) -> String {
    match err {
        ClickUpError::Unauthorized => "ClickUp rejected your token.".to_string(),
        ClickUpError::NotFound => {
            "Access denied, or ClickUp no longer shows this task.".to_string()
        }
        ClickUpError::Api { status: 403, .. } => {
            "Access denied: your ClickUp account cannot comment on this task.".to_string()
        }
        other => other.to_string(),
    }
}

/// Whether `comments` already include the pointer.
fn has_pointer(comments: &[ClickUpComment]) -> bool {
    comments
        .iter()
        .any(|comment| copy_text::is_pointer_comment(&comment.text))
}

/// Sets `task_id` to the complete status of `list_id` (statuses differ per
/// list, so they are read from it, never assumed).
async fn complete_task(
    client: &ClickUpClient,
    limiter: &RateLimiter,
    token: &str,
    task_id: &str,
    list_id: &str,
) -> Outcome {
    limiter.acquire().await;
    let statuses = match client.list_statuses(token, list_id).await {
        Ok(statuses) => statuses,
        Err(err) => {
            return Outcome::failed(format!(
                "Could not read the list's statuses: {}",
                failure_message(&err)
            ))
        }
    };
    let Some(status) = completion_status(&statuses) else {
        return Outcome::failed("This list has no complete status.");
    };

    limiter.acquire().await;
    match client.set_status(token, task_id, &status.name).await {
        Ok(()) => Outcome::done(),
        Err(err) => Outcome::failed(failure_message(&err)),
    }
}

/// Posts `comment` on `task_id` (ending with the source-task footer when
/// `source` is known), then -- if it went through, a pointer applies and
/// the task does not have one yet -- the pointer, then -- when
/// `complete_in_list` names the task's list -- sets the task complete.
/// Completing is only attempted once the comment is posted.
#[allow(clippy::too_many_arguments)]
pub(super) async fn copy_one(
    client: &ClickUpClient,
    limiter: &RateLimiter,
    token: &str,
    task_id: &str,
    comment: &str,
    source: Option<&SourceLink>,
    pointer_list: Option<&ListInfo>,
    complete_in_list: Option<&str>,
) -> ItemResult {
    limiter.acquire().await;
    let parts = copy_text::comment_parts(
        comment.trim(),
        source.map(|link| (link.name.as_str(), link.url.as_str())),
    );
    let comment = match client.add_comment(token, task_id, &parts).await {
        Ok(()) => Outcome::done(),
        Err(err) => Outcome::failed(failure_message(&err)),
    };

    // No comment, no pointer: the pointer explains where a copied comment
    // came from, and a row that failed should be retried whole.
    let pointer = match pointer_list.filter(|_| comment.ok) {
        None => PointerOutcome::state("not_applicable"),
        Some(list) => {
            limiter.acquire().await;
            match client.task_comments(token, task_id).await {
                Err(err) => PointerOutcome::failed(format!(
                    "Could not check for the main-list note: {}",
                    failure_message(&err)
                )),
                Ok(existing) if has_pointer(&existing) => PointerOutcome::state("already_present"),
                Ok(_) => {
                    limiter.acquire().await;
                    match client
                        .add_comment(
                            token,
                            task_id,
                            &copy_text::pointer_parts(&list.list_name, &list.list_url),
                        )
                        .await
                    {
                        Ok(()) => PointerOutcome::state("posted"),
                        Err(err) => PointerOutcome::failed(failure_message(&err)),
                    }
                }
            }
        }
    };

    let completed = match complete_in_list.filter(|_| comment.ok) {
        Some(list_id) => Some(complete_task(client, limiter, token, task_id, list_id).await),
        None => None,
    };

    ItemResult {
        target_task_id: task_id.to_string(),
        comment,
        pointer,
        completed,
        facility_id: None,
        facility_name: None,
    }
}

/// One destination of a bulk copy: a task in one facility's list, and the
/// main list its pointer should name (none when this is that list's own
/// facility, or no parent is designated).
pub(super) struct BulkItem {
    pub facility: ListInfo,
    pub target_task_id: String,
    pub pointer: Option<ListInfo>,
}

/// Rows written at once.
pub(super) const CHUNK_SIZE: usize = 4;

/// Posts `comment` to each of `chunk`'s tasks at once (each row already
/// waits on the rate limiter), tagging every result with its facility.
pub(super) async fn copy_chunk(
    client: &ClickUpClient,
    limiter: &RateLimiter,
    token: &str,
    comment: &str,
    source: Option<&SourceLink>,
    complete: bool,
    chunk: &[BulkItem],
) -> Vec<ItemResult> {
    futures::future::join_all(chunk.iter().map(|item| async move {
        let mut result = copy_one(
            client,
            limiter,
            token,
            &item.target_task_id,
            comment,
            source,
            item.pointer.as_ref(),
            complete.then_some(item.facility.list_id.as_str()),
        )
        .await;
        result.facility_id = Some(item.facility.facility_id);
        result.facility_name = Some(item.facility.facility_name.clone());
        result
    }))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_costs_one_call_without_a_pointer_and_three_with() {
        assert_eq!(estimated_calls(10, false, false), 10);
        assert_eq!(estimated_calls(10, true, false), 30);
    }

    #[test]
    fn completing_adds_two_calls_a_row() {
        assert_eq!(estimated_calls(10, false, true), 30);
        assert_eq!(estimated_calls(10, true, true), 50);
    }

    #[test]
    fn about_sixteen_rows_with_pointers_fit_the_inline_budget() {
        assert!(estimated_calls(16, true, false) <= INLINE_CALL_BUDGET);
        assert!(estimated_calls(17, true, false) > INLINE_CALL_BUDGET);
    }

    #[test]
    fn ten_rows_with_pointers_and_completing_fit_the_inline_budget() {
        assert!(estimated_calls(10, true, true) <= INLINE_CALL_BUDGET);
        assert!(estimated_calls(11, true, true) > INLINE_CALL_BUDGET);
    }
}
