//! The client's bulk copy: one comment copied from a task in a source
//! facility's list to the counterpart task in several other facilities'
//! lists at once.
//!
//! - `bulk-tasks`: the source list's tasks in every Onboarding Phase, and
//!   which other facilities could be destinations;
//! - `bulk-pairs`: for the chosen source task, each destination's
//!   suggested counterpart (and its tasks, to choose another by hand);
//! - `bulk-comment`: the source task's latest comment, to prefill;
//! - `bulk-copy`: posts the comment to the chosen destinations -- inside
//!   the request when it is small, otherwise as a background job the
//!   person is told about when it finishes (see `jobs`).
//!
//! Like the facility dialog it runs on the caller's own ClickUp token,
//! through that user's rate limiter, and posts the once-per-task pointer.

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Json, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::comments::SourceComment;
use super::exec::{
    copy_chunk, estimated_calls, BulkItem, ItemResult, SourceLink, CHUNK_SIZE, INLINE_CALL_BUDGET,
};
use super::jobs;
use super::lists::{
    list_info, load_tasks_lenient, parse_scope, valid_task_id, FacilityList, FacilityRow, ListInfo,
    MAX_COMMENT_CHARS, PERMISSION,
};
use super::pairs::{target_choice, task_info, TargetChoice, TaskInfo};
use crate::api::clickup_connection::{clickup_client, load_user_token};
use crate::api::rls::try_response;
use crate::api::{bad_request, conflict, internal_error, not_found, user_agent_from, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clickup::comments::latest;
use crate::clickup::copy_pairing;
use crate::clickup::rate_limit;
use crate::clickup::task_cache;
use crate::clickup::task_matching;
use crate::client_ops::audit_log;

/// Destinations in one bulk copy; stops an absurd request.
const MAX_DESTINATIONS: usize = 100;

/// Destination lists read at once.
const LIST_READ_CONCURRENCY: usize = 4;

struct BulkContext {
    source: ListInfo,
    parent: Option<ListInfo>,
    /// Every other facility with a linked list.
    destinations: Vec<ListInfo>,
    /// Facilities with no list linked yet (named, so the page can say so).
    unlinked: Vec<(Uuid, String)>,
}

/// The company's facilities sorted into source, parent, destinations and
/// unlinked, or the response to return instead.
async fn prepare_bulk(
    state: &AppState,
    user: &AuthenticatedUser,
    company_id: Uuid,
    source_facility_id: Option<Uuid>,
) -> Result<BulkContext, Response> {
    let fail = |err: sqlx::Error| {
        tracing::error!(error = %err, user_id = %user.user_id, "ClickUp bulk copy lookup failed");
        internal_error("Could not prepare ClickUp Copy")
    };

    let mut tx = begin_rls_transaction(&state.db, user.user_id, &user.role_keys)
        .await
        .map_err(fail)?;
    let parent_id: Option<Option<Uuid>> = sqlx::query_scalar(
        "SELECT clickup_parent_facility_id FROM clients.companies WHERE id = $1",
    )
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(fail)?;
    let rows: Vec<FacilityRow> = sqlx::query_as(
        "SELECT id, name, clickup_list_id, clickup_list_name, clickup_list_url \
           FROM clients.facilities WHERE company_id = $1 ORDER BY name",
    )
    .bind(company_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(fail)?;
    tx.commit().await.map_err(fail)?;

    let Some(parent_id) = parent_id else {
        return Err(not_found("not_found", "No such client.".to_string()));
    };

    let Some(source_id) = source_facility_id.or(parent_id) else {
        return Err(conflict(
            "no_source_facility",
            "Choose which facility to copy from, or designate a parent facility on the Company page."
                .to_string(),
        ));
    };

    let Some(source_row) = rows.iter().find(|row| row.0 == source_id).cloned() else {
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

    let parent = parent_id
        .and_then(|id| rows.iter().find(|row| row.0 == id).cloned())
        .and_then(list_info);

    let mut destinations = Vec::new();
    let mut unlinked = Vec::new();
    for row in rows.into_iter().filter(|row| row.0 != source_id) {
        let (id, name) = (row.0, row.1.clone());
        match list_info(row) {
            Some(list) => destinations.push(list),
            None => unlinked.push((id, name)),
        }
    }

    Ok(BulkContext {
        source,
        parent,
        destinations,
        unlinked,
    })
}

// ---------------------------------------------------------------- tasks

#[derive(Debug, Deserialize)]
pub struct BulkTasksQuery {
    pub source_facility_id: Option<Uuid>,
    /// `corporate`, `facility` or `all` (the default).
    pub scope: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FacilityRef {
    pub facility_id: Uuid,
    pub facility_name: String,
}

#[derive(Debug, Serialize)]
pub struct BulkTask {
    pub phase: String,
    /// The phase's position in the template's own order.
    pub phase_order: i64,
    #[serde(flatten)]
    pub task: TaskInfo,
}

#[derive(Debug, Serialize)]
pub struct BulkTasksResponse {
    pub source: FacilityList,
    /// The company's main list, named by the pointer comment.
    pub parent: Option<FacilityList>,
    pub tasks: Vec<BulkTask>,
    /// Facilities the comment can be copied to.
    pub destinations: Vec<FacilityRef>,
    /// Facilities with no ClickUp list linked yet, so not offered.
    pub unlinked: Vec<FacilityRef>,
}

/// `GET /clients/{company_id}/clickup/bulk-tasks`
pub async fn bulk_tasks(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
    Query(query): Query<BulkTasksQuery>,
) -> Response {
    try_response!(
        user.require_permission(&state.db, PERMISSION, "clickup_bulk_tasks", None, None)
            .await
    );
    let scope = match parse_scope(query.scope.as_deref()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

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
    let parents = task_matching::parent_names(&source_tasks);

    let tasks = copy_pairing::eligible(&source_tasks, scope)
        .into_iter()
        .map(|task| BulkTask {
            phase: copy_pairing::copy_phase(task)
                .unwrap_or_default()
                .to_string(),
            phase_order: copy_pairing::phase_order(task),
            task: task_info(task, &parents),
        })
        .collect();

    let facility_ref = |id: Uuid, name: &str| FacilityRef {
        facility_id: id,
        facility_name: name.to_string(),
    };
    Json(BulkTasksResponse {
        source: (&ctx.source).into(),
        parent: ctx.parent.as_ref().map(FacilityList::from),
        tasks,
        destinations: ctx
            .destinations
            .iter()
            .map(|d| facility_ref(d.facility_id, &d.facility_name))
            .collect(),
        unlinked: ctx
            .unlinked
            .iter()
            .map(|(id, name)| facility_ref(*id, name))
            .collect(),
    })
    .into_response()
}

// ---------------------------------------------------------------- pairs

#[derive(Debug, Deserialize)]
pub struct BulkPairsQuery {
    pub source_facility_id: Option<Uuid>,
    pub source_task_id: String,
    pub scope: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DestinationPairs {
    pub facility_id: Uuid,
    pub facility_name: String,
    pub list_name: String,
    pub list_url: String,
    /// The suggested counterpart of the source task; `None` is "no match".
    pub target: Option<TargetChoice>,
    pub alternatives: Vec<TargetChoice>,
    /// Every eligible task in this facility's list, to choose by hand.
    pub tasks: Vec<TaskInfo>,
    /// Why this facility's list could not be read, when it could not.
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BulkPairsResponse {
    pub destinations: Vec<DestinationPairs>,
}

/// `GET /clients/{company_id}/clickup/bulk-pairs`
pub async fn bulk_pairs(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
    Query(query): Query<BulkPairsQuery>,
) -> Response {
    try_response!(
        user.require_permission(&state.db, PERMISSION, "clickup_bulk_pairs", None, None)
            .await
    );
    let source_task_id = query.source_task_id.trim().to_string();
    if !valid_task_id(&source_task_id) {
        return bad_request("invalid_clickup_task", "Choose a ClickUp task.".to_string());
    }
    let scope = match parse_scope(query.scope.as_deref()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

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

    // A few lists at a time; each is cached for later requests.
    let mut destinations = Vec::with_capacity(ctx.destinations.len());
    for chunk in ctx.destinations.chunks(LIST_READ_CONCURRENCY) {
        let loaded = futures::future::join_all(
            chunk
                .iter()
                .map(|list| load_tasks_lenient(&state, &user, &token, list)),
        )
        .await;

        for (list, loaded) in chunk.iter().zip(loaded) {
            let mut entry = DestinationPairs {
                facility_id: list.facility_id,
                facility_name: list.facility_name.clone(),
                list_name: list.list_name.clone(),
                list_url: list.list_url.clone(),
                target: None,
                alternatives: Vec::new(),
                tasks: Vec::new(),
                error: None,
            };

            match loaded {
                Err(message) => entry.error = Some(message),
                Ok(tasks) => {
                    let parents = task_matching::parent_names(&tasks);
                    if let Some(pairing) = copy_pairing::pair(&source_tasks, &tasks, scope)
                        .into_iter()
                        .find(|pairing| pairing.source.id == source_task_id)
                    {
                        entry.target = pairing.target.as_ref().map(|c| target_choice(c, &parents));
                        entry.alternatives = pairing
                            .alternatives
                            .iter()
                            .map(|c| target_choice(c, &parents))
                            .collect();
                    }
                    entry.tasks = copy_pairing::eligible(&tasks, scope)
                        .into_iter()
                        .map(|task| task_info(task, &parents))
                        .collect();
                }
            }
            destinations.push(entry);
        }
    }

    Json(BulkPairsResponse { destinations }).into_response()
}

// -------------------------------------------------------------- comment

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
        Err(err) => return super::comments::comment_reads(&err),
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

// ----------------------------------------------------------------- copy

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

/// After the writes: the cached lists changed, and each destination
/// facility's activity log records what was copied onto it.
#[allow(clippy::too_many_arguments)]
async fn finish_copy(
    state: &AppState,
    user_id: Uuid,
    company_id: Uuid,
    source_facility_id: Uuid,
    items: &[BulkItem],
    results: &[ItemResult],
    complete_requested: bool,
    user_agent: Option<&str>,
) {
    for list_id in items
        .iter()
        .map(|item| item.facility.list_id.as_str())
        .collect::<HashSet<_>>()
    {
        task_cache::invalidate_list(list_id);
    }

    let mut per_facility: HashMap<Uuid, (usize, usize, usize, usize)> = HashMap::new();
    for result in results {
        if let Some(id) = result.facility_id {
            let entry = per_facility.entry(id).or_default();
            if result.comment.ok {
                entry.0 += 1;
            } else {
                entry.1 += 1;
            }
            if result.pointer.state == "posted" {
                entry.2 += 1;
            }
            if result.completed.as_ref().is_some_and(|outcome| outcome.ok) {
                entry.3 += 1;
            }
        }
    }
    for (facility_id, (copied, failed, pointers, completed)) in per_facility {
        audit_log::record(
            &state.db,
            audit_log::event::FACILITY_CLICKUP_COMMENTS_COPIED,
            user_id,
            "facility",
            Some(&facility_id.to_string()),
            audit_log::Change::none(),
            user_agent,
            None,
            serde_json::json!({
                "company_id": company_id,
                "source_facility_id": source_facility_id,
                "bulk": true,
                "copied": copied,
                "failed": failed,
                "pointers_posted": pointers,
                "complete_requested": complete_requested,
                "tasks_completed": completed,
            }),
        )
        .await;
    }
}

/// Runs a bulk copy in the background, saving progress after each batch.
#[allow(clippy::too_many_arguments)]
async fn run_job(
    state: AppState,
    user_id: Uuid,
    role_keys: Vec<String>,
    token: String,
    comment: String,
    source: Option<SourceLink>,
    complete: bool,
    items: Vec<BulkItem>,
    job_id: Uuid,
    company_id: Uuid,
    source_facility_id: Uuid,
    user_agent: Option<String>,
) {
    let client = clickup_client(&state);
    let limiter = rate_limit::for_user(user_id);

    let mut results: Vec<ItemResult> = Vec::with_capacity(items.len());
    for chunk in items.chunks(CHUNK_SIZE) {
        results.extend(
            copy_chunk(
                &client,
                &limiter,
                &token,
                &comment,
                source.as_ref(),
                complete,
                chunk,
            )
            .await,
        );
        jobs::record_progress(&state.db, user_id, &role_keys, job_id, &results).await;
    }

    finish_copy(
        &state,
        user_id,
        company_id,
        source_facility_id,
        &items,
        &results,
        complete,
        user_agent.as_deref(),
    )
    .await;
    jobs::finish(&state.db, user_id, &role_keys, job_id, "done", None).await;
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

    if estimated_calls(total, ctx.parent.is_some(), complete) <= INLINE_CALL_BUDGET {
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
