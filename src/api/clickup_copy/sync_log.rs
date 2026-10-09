//! The facility's "Last Synced Project" log: which facility's ClickUp
//! list comments were copied from onto this facility's tasks, when, by
//! whom, and how it went.
//!
//! There is no table of its own. Every copy already writes a
//! `facility_clickup_comments_copied` row in `client_ops.audit_log` (the
//! Activity Logs trail) against the *target* facility -- single copies,
//! bulk copies and background jobs alike -- so this reads that trail
//! instead of keeping a second copy that could disagree with it. The
//! Activity Logs page shows the same rows with every other event; this
//! is the same data narrowed to one facility and one event type.

use axum::{
    extract::{Json, Path, Query, State},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::lists::PERMISSION;
use crate::api::paging::clamp_limit;
use crate::api::rls::{begin_for, try_response};
use crate::api::tool_runs::facility_belongs_to_company;
use crate::api::{internal_error, not_found, AppState};
use crate::auth::AuthenticatedUser;
use crate::client_ops::audit_log;

const DEFAULT_LIMIT: i64 = 20;
const MAX_LIMIT: i64 = 100;

#[derive(Debug, Deserialize)]
pub struct SyncLogQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    /// Keyset paging, as the Activity Logs listing: only rows older than
    /// this entry (ids are time-ordered `uuidv7`s).
    #[serde(default)]
    pub before_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct SyncLogEntry {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    /// Who copied; `None` when their name is not readable by the caller.
    pub actor_name: Option<String>,
    pub source_facility_id: Option<Uuid>,
    /// The source facility's current name, `None` if it was deleted.
    pub source_facility_name: Option<String>,
    pub copied: i64,
    pub failed: i64,
    /// Came from the client's bulk tab rather than this facility's dialog.
    pub bulk: bool,
    pub pointers_posted: i64,
    pub complete_requested: bool,
    pub tasks_completed: i64,
}

#[derive(Debug, Serialize)]
pub struct SyncLogResponse {
    /// Newest first.
    pub entries: Vec<SyncLogEntry>,
    /// Whether older entries exist beyond these.
    pub has_more: bool,
}

type Row = (
    Uuid,
    DateTime<Utc>,
    Option<String>,
    Option<String>,
    serde_json::Value,
    Option<String>,
);

fn count(metadata: &serde_json::Value, key: &str) -> i64 {
    metadata.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

/// The copies made onto this facility's tasks, newest first.
pub async fn facility_sync_log(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<SyncLogQuery>,
) -> Response {
    try_response!(
        user.require_permission(&state.db, PERMISSION, "clickup_sync_log", None, None)
            .await
    );

    let limit = clamp_limit(query.limit, DEFAULT_LIMIT, MAX_LIMIT);
    let mut tx = try_response!(begin_for(&state, &user, "Could not load the sync log").await);

    match facility_belongs_to_company(&mut tx, facility_id, company_id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = tx.commit().await;
            return not_found("not_found", "No such facility.".to_string());
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for the sync log failed");
            return internal_error("Could not load the sync log");
        }
    }

    // One extra row tells whether there is a next page. The source name is
    // looked up now (not snapshotted): a renamed facility reads correctly,
    // a deleted one has no row to join and reads as removed.
    let rows: Result<Vec<Row>, sqlx::Error> = sqlx::query_as(
        "SELECT a.id, a.created_at,
                NULLIF(btrim(concat_ws(' ', u.first_name, u.last_name)), ''),
                a.metadata->>'source_facility_id', a.metadata, f.name
           FROM client_ops.audit_log a
           LEFT JOIN auth.users u ON u.id = a.actor_user_id
           LEFT JOIN clients.facilities f ON f.id::text = a.metadata->>'source_facility_id'
          WHERE a.event_type = $1 AND a.entity_type = 'facility' AND a.entity_id = $2
            AND ($3::uuid IS NULL OR a.id < $3)
          ORDER BY a.id DESC
          LIMIT $4",
    )
    .bind(audit_log::event::FACILITY_CLICKUP_COMMENTS_COPIED)
    .bind(facility_id.to_string())
    .bind(query.before_id)
    .bind(limit + 1)
    .fetch_all(&mut *tx)
    .await;

    let mut rows = match rows {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "sync log query failed");
            return internal_error("Could not load the sync log");
        }
    };
    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit the sync log transaction");
        return internal_error("Could not load the sync log");
    }

    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);

    let entries = rows
        .into_iter()
        .map(
            |(id, created_at, actor_name, source_id, metadata, source_name)| SyncLogEntry {
                id,
                created_at,
                actor_name,
                source_facility_id: source_id.and_then(|raw| Uuid::parse_str(&raw).ok()),
                source_facility_name: source_name,
                copied: count(&metadata, "copied"),
                failed: count(&metadata, "failed"),
                bulk: metadata
                    .get("bulk")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                pointers_posted: count(&metadata, "pointers_posted"),
                complete_requested: metadata
                    .get("complete_requested")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                tasks_completed: count(&metadata, "tasks_completed"),
            },
        )
        .collect();

    Json(SyncLogResponse { entries, has_more }).into_response()
}
