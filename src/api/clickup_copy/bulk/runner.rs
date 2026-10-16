//! Finishing a bulk copy: the activity-log records and cache invalidation
//! after the writes, and the background job runner.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::super::exec::{copy_chunk, BulkItem, ItemResult, SourceLink, CHUNK_SIZE};
use super::super::jobs;
use crate::api::clickup_connection::clickup_client;
use crate::api::AppState;
use crate::clickup::rate_limit;
use crate::clickup::task_cache;
use crate::client_ops::audit_log;

/// After the writes: the cached lists changed, and each destination
/// facility's activity log records what was copied onto it.
#[allow(clippy::too_many_arguments)]
pub(super) async fn finish_copy(
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
pub(super) async fn run_job(
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
    let copied = results.iter().filter(|r| r.comment.ok).count();
    tracing::info!(%job_id, total = results.len(), copied, failed = results.len() - copied, "ClickUp Copy job results");
    jobs::finish(&state.db, user_id, &role_keys, job_id, "done", None).await;
}
