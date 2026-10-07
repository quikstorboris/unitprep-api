//! ClickUp Copy: copying a comment from a task in one facility's ClickUp
//! list to its counterpart task in another facility's list.
//!
//! Three endpoints, all on the **target** facility (the page the person
//! is on), with the **source** facility chosen per request and defaulting
//! to the company's designated parent:
//!
//! - `copy-pairs`: the source and target lists' tasks in the Set Up and
//!   Migration phases, paired by name/phase/parent (suggestions only);
//! - `copy-comments`: one row's source comment (the prefill) and whether
//!   the target looks as if it already has it -- fetched per row so the
//!   dialog does not read every task's comments up front;
//! - `copy`: posts the (edited) comments, plus -- once per target task --
//!   a generic pointer comment naming the company's main list.
//!
//! Orchestrator stores nothing about what was copied: ClickUp is the
//! record, and the "already copied" / "pointer already there" checks read
//! the target task's comments (see `clickup::copy_text`). Everything runs
//! with the caller's own ClickUp token, so ClickUp attributes the
//! comments to the person who clicked.
//!
//! Until the background queue exists, one request is capped at
//! [`MAX_ITEMS`] rows, which keeps it inside ClickUp's ~100 requests a
//! minute even when every row also needs a pointer comment.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Json, Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::clickup_connection::{clickup_client, clickup_failure_response, load_user_token};
use crate::api::tool_runs::facility_belongs_to_company;
use crate::api::{bad_request, conflict, internal_error, not_found, user_agent_from, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clickup::comments::{latest, ClickUpComment};
use crate::clickup::copy_pairing::{self, Scope};
use crate::clickup::copy_text;
use crate::clickup::task_cache;
use crate::clickup::task_matching;
use crate::clickup::tasks::ClickUpTask;
use crate::clickup::{ClickUpClient, ClickUpError};
use crate::client_ops::audit_log;

const PERMISSION: &str = "integrations.clickup";

/// Rows per copy request, until background execution exists.
const MAX_ITEMS: usize = 30;

/// A ClickUp comment is not worth more than this; stops an absurd body.
const MAX_COMMENT_CHARS: usize = 10_000;

/// Rows written at once.
const WRITE_CONCURRENCY: usize = 4;

/// One facility's ClickUp list.
#[derive(Debug, Clone)]
struct ListInfo {
    facility_id: Uuid,
    facility_name: String,
    list_id: String,
    list_name: String,
    list_url: String,
}

#[derive(Debug, Serialize)]
pub struct FacilityList {
    pub facility_id: Uuid,
    pub facility_name: String,
    pub list_name: String,
    pub list_url: String,
}

impl From<&ListInfo> for FacilityList {
    fn from(list: &ListInfo) -> Self {
        Self {
            facility_id: list.facility_id,
            facility_name: list.facility_name.clone(),
            list_name: list.list_name.clone(),
            list_url: list.list_url.clone(),
        }
    }
}

struct Prepared {
    target: ListInfo,
    source: ListInfo,
    /// The company's designated parent, when it has a linked list: what
    /// the pointer comment names.
    parent: Option<ListInfo>,
}

type FacilityRow = (Uuid, String, Option<String>, Option<String>, Option<String>);

fn list_info(row: FacilityRow) -> Option<ListInfo> {
    let (facility_id, facility_name, list_id, list_name, list_url) = row;
    Some(ListInfo {
        facility_id,
        facility_name,
        list_id: list_id?,
        list_name: list_name?,
        list_url: list_url?,
    })
}

fn server_error(context: &'static str, err: &sqlx::Error, user: &AuthenticatedUser) -> Response {
    tracing::error!(error = %err, user_id = %user.user_id, "{context}");
    internal_error("Could not prepare ClickUp Copy")
}

/// Reads the target, source and parent facilities' lists, or the response
/// to return instead.
async fn prepare(
    state: &AppState,
    user: &AuthenticatedUser,
    company_id: Uuid,
    facility_id: Uuid,
    source_facility_id: Option<Uuid>,
) -> Result<Prepared, Response> {
    let fail = |err: sqlx::Error| server_error("ClickUp Copy lookup failed", &err, user);

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

    let parent_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT clickup_parent_facility_id FROM clients.companies WHERE id = $1",
    )
    .bind(company_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(fail)?;

    let Some(source_id) = source_facility_id.or(parent_id) else {
        let _ = tx.commit().await;
        return Err(conflict(
            "no_source_facility",
            "Choose which facility to copy from, or designate a parent facility on the Company page."
                .to_string(),
        ));
    };
    if source_id == facility_id {
        let _ = tx.commit().await;
        return Err(bad_request(
            "source_is_target",
            "Choose a different facility to copy from: this is the one being copied to."
                .to_string(),
        ));
    }

    let wanted: Vec<Uuid> = [Some(facility_id), Some(source_id), parent_id]
        .into_iter()
        .flatten()
        .collect();
    let rows: Vec<FacilityRow> = sqlx::query_as(
        "SELECT id, name, clickup_list_id, clickup_list_name, clickup_list_url \
           FROM clients.facilities WHERE company_id = $1 AND id = ANY($2)",
    )
    .bind(company_id)
    .bind(&wanted)
    .fetch_all(&mut *tx)
    .await
    .map_err(fail)?;
    tx.commit().await.map_err(fail)?;

    let by_id: HashMap<Uuid, FacilityRow> = rows.into_iter().map(|row| (row.0, row)).collect();

    let Some(source_row) = by_id.get(&source_id).cloned() else {
        return Err(bad_request(
            "invalid_source_facility",
            "That facility does not belong to this client.".to_string(),
        ));
    };
    let source_name = source_row.1.clone();
    let Some(source) = list_info(source_row) else {
        return Err(conflict(
            "source_not_linked_to_clickup",
            format!("{source_name} has no ClickUp list linked, so there is nothing to copy from."),
        ));
    };

    let target_row = by_id
        .get(&facility_id)
        .cloned()
        .ok_or_else(|| not_found("not_found", "No such facility.".to_string()))?;
    let Some(target) = list_info(target_row) else {
        return Err(conflict(
            "facility_not_linked_to_clickup",
            "Link this facility to its ClickUp list first (the ClickUp section on its General tab)."
                .to_string(),
        ));
    };

    // The parent may be the source, the target or a third facility, and
    // has no pointer to name if it has no linked list.
    let parent = parent_id
        .and_then(|id| by_id.get(&id).cloned())
        .and_then(list_info);

    Ok(Prepared {
        target,
        source,
        parent,
    })
}

/// A list's tasks (cached for a few minutes, shared with the duplicate
/// check), or the response to return instead.
async fn load_tasks(
    state: &AppState,
    user: &AuthenticatedUser,
    token: &str,
    list: &ListInfo,
) -> Result<Arc<Vec<ClickUpTask>>, Response> {
    match task_cache::get_or_load(user.user_id, &list.list_id, || async {
        clickup_client(state).list_tasks(token, &list.list_id).await
    })
    .await
    {
        Ok(tasks) => Ok(tasks),
        Err(ClickUpError::NotFound) => Err(not_found(
            "clickup_list_not_found",
            format!(
                "ClickUp no longer shows the linked list \"{}\". Re-link {}.",
                list.list_name, list.facility_name
            ),
        )),
        Err(err) => Err(clickup_failure_response(state, user, &err).await),
    }
}

/// The Corp/Fac filter from the query string. The error is the ready-made
/// 400 response, like the other helpers here (and why the lint is allowed).
#[allow(clippy::result_large_err)]
fn parse_scope(raw: Option<&str>) -> Result<Option<Scope>, Response> {
    match raw.map(str::trim) {
        None | Some("") | Some("all") => Ok(None),
        Some("corporate") => Ok(Some(Scope::Corporate)),
        Some("facility") => Ok(Some(Scope::Facility)),
        Some(_) => Err(bad_request(
            "invalid_scope",
            "Scope must be corporate, facility or all.".to_string(),
        )),
    }
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Corporate => "corporate",
        Scope::Facility => "facility",
    }
}

fn valid_task_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

// ---------------------------------------------------------------- pairs

#[derive(Debug, Deserialize)]
pub struct PairsQuery {
    pub source_facility_id: Option<Uuid>,
    /// `corporate`, `facility` or `all` (the default).
    pub scope: Option<String>,
}

/// A task as the dialog shows it.
#[derive(Debug, Serialize)]
pub struct TaskInfo {
    pub task_id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub parent_name: Option<String>,
    pub status: String,
    pub is_finished: bool,
    pub url: String,
    /// `corporate` / `facility`, when the task's Corp/Fac says.
    pub scope: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct TargetChoice {
    #[serde(flatten)]
    pub task: TaskInfo,
    pub score: f64,
}

#[derive(Debug, Serialize)]
pub struct PairRow {
    /// The phase ("Set Up", "Migration") the dialog groups rows under.
    pub phase: String,
    pub source: TaskInfo,
    /// The suggested counterpart; `None` is "no match".
    pub target: Option<TargetChoice>,
    pub alternatives: Vec<TargetChoice>,
}

#[derive(Debug, Serialize)]
pub struct PairsResponse {
    pub source: FacilityList,
    pub target: FacilityList,
    /// The company's main list, named by the pointer comment; `None` when
    /// no parent is designated (no pointer is posted then).
    pub parent: Option<FacilityList>,
    pub rows: Vec<PairRow>,
    /// Every eligible task in the target list, for choosing a different
    /// counterpart by hand.
    pub target_tasks: Vec<TaskInfo>,
}

fn task_info(task: &ClickUpTask, parents: &HashMap<&str, &str>) -> TaskInfo {
    TaskInfo {
        task_id: task.id.clone(),
        name: task.name.trim().to_string(),
        parent_id: task.parent_id.clone(),
        parent_name: task
            .parent_id
            .as_deref()
            .and_then(|id| parents.get(id))
            .map(|name| name.trim().to_string()),
        status: task.status.clone(),
        is_finished: task.is_finished(),
        url: task.url.clone(),
        scope: copy_pairing::scope(task).map(scope_name),
    }
}

fn target_choice(
    candidate: &copy_pairing::Candidate<'_>,
    parents: &HashMap<&str, &str>,
) -> TargetChoice {
    TargetChoice {
        task: task_info(candidate.task, parents),
        score: (candidate.score * 100.0).round() / 100.0,
    }
}

/// `GET .../clickup/copy-pairs`
pub async fn copy_pairs(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<PairsQuery>,
) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "clickup_copy_pairs", None, None)
        .await
    {
        return response;
    }
    let scope = match parse_scope(query.scope.as_deref()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

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

    let (source_tasks, target_tasks) = match tokio::try_join!(
        load_tasks(&state, &user, &token, &prepared.source),
        load_tasks(&state, &user, &token, &prepared.target)
    ) {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    let source_parents = task_matching::parent_names(&source_tasks);
    let target_parents = task_matching::parent_names(&target_tasks);

    let rows = copy_pairing::pair(&source_tasks, &target_tasks, scope)
        .into_iter()
        .map(|pairing| PairRow {
            phase: copy_pairing::copy_phase(pairing.source)
                .unwrap_or_default()
                .to_string(),
            source: task_info(pairing.source, &source_parents),
            target: pairing
                .target
                .as_ref()
                .map(|c| target_choice(c, &target_parents)),
            alternatives: pairing
                .alternatives
                .iter()
                .map(|c| target_choice(c, &target_parents))
                .collect(),
        })
        .collect();

    Json(PairsResponse {
        source: (&prepared.source).into(),
        target: (&prepared.target).into(),
        parent: prepared.parent.as_ref().map(FacilityList::from),
        rows,
        target_tasks: copy_pairing::eligible(&target_tasks, scope)
            .into_iter()
            .map(|task| task_info(task, &target_parents))
            .collect(),
    })
    .into_response()
}

// ------------------------------------------------------------- comments

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

fn comment_reads(err: &ClickUpError) -> Response {
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

// ----------------------------------------------------------------- copy

#[derive(Debug, Deserialize)]
pub struct CopyItem {
    /// The target task the comment is posted on.
    pub target_task_id: String,
    /// The comment as the person edited it.
    pub comment: String,
}

#[derive(Debug, Deserialize)]
pub struct CopyRequest {
    pub source_facility_id: Option<Uuid>,
    pub items: Vec<CopyItem>,
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

    fn failed(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: Some(message.into()),
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
    fn state(state: &'static str) -> Self {
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

#[derive(Debug, Serialize)]
pub struct ItemResult {
    pub target_task_id: String,
    pub comment: Outcome,
    pub pointer: PointerOutcome,
}

#[derive(Debug, Serialize)]
pub struct CopyResponse {
    pub results: Vec<ItemResult>,
    pub copied: usize,
    pub failed: usize,
}

fn failure_message(err: &ClickUpError) -> String {
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

/// Posts one row's comment, then -- if it went through and the task does
/// not have it yet -- the pointer.
async fn copy_one(
    client: &ClickUpClient,
    token: &str,
    item: &CopyItem,
    pointer_list: Option<&ListInfo>,
) -> ItemResult {
    let task_id = item.target_task_id.trim();

    let comment = match client
        .add_comment(token, task_id, &[(item.comment.trim(), None)])
        .await
    {
        Ok(()) => Outcome::done(),
        Err(err) => Outcome::failed(failure_message(&err)),
    };

    // No comment, no pointer: the pointer explains where a copied comment
    // came from, and a row that failed should be retried whole.
    let pointer = match pointer_list.filter(|_| comment.ok) {
        None => PointerOutcome::state("not_applicable"),
        Some(list) => match client.task_comments(token, task_id).await {
            Err(err) => PointerOutcome::failed(format!(
                "Could not check for the main-list note: {}",
                failure_message(&err)
            )),
            Ok(existing) if has_pointer(&existing) => PointerOutcome::state("already_present"),
            Ok(_) => match client
                .add_comment(
                    token,
                    task_id,
                    &copy_text::pointer_parts(&list.list_name, &list.list_url),
                )
                .await
            {
                Ok(()) => PointerOutcome::state("posted"),
                Err(err) => PointerOutcome::failed(failure_message(&err)),
            },
        },
    };

    ItemResult {
        target_task_id: task_id.to_string(),
        comment,
        pointer,
    }
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

    // The parent's own tasks need no pointer to the parent.
    let pointer_list = prepared
        .parent
        .as_ref()
        .filter(|parent| parent.facility_id != prepared.target.facility_id);

    let client = clickup_client(&state);
    // A few rows at a time, in the order given.
    let mut results: Vec<ItemResult> = Vec::with_capacity(request.items.len());
    for chunk in request.items.chunks(WRITE_CONCURRENCY) {
        results.extend(
            futures::future::join_all(
                chunk
                    .iter()
                    .map(|item| copy_one(&client, &token, item, pointer_list)),
            )
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
