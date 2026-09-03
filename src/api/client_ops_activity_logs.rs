//! Activity Logs listing -- the backend half of the Administration >
//! Activity Logs viewer, first-ever UI surface for `client_ops::audit_log`
//! (previously write-only, used only for QMS tag edits). Distinct from
//! `auth_audit_logs` (renamed "Security Logs" in the nav): that one is the
//! security audit trail (logins, role changes, authorization failures),
//! gated by `audit_logs.read` and admin-oriented; this one is an
//! operations trail for the people doing the operations -- imports, dedup/
//! Unit Group runs, Process Street syncs -- gated by the separate
//! `activity_logs.read` permission, granted to the same three client-ops
//! roles that can already `SELECT client_ops.audit_log` under RLS (see
//! that table's own migration comment).
//!
//! Mirrors `auth_audit_logs`'s shape closely (same keyset-pagination
//! idiom, same query-builder filter style) but is not literally shared
//! with it: the underlying tables differ in exactly the ways that make a
//! shared abstraction not worth it here -- `id` is a time-ordered UUID
//! (`uuidv7`) rather than a bigint identity column, and rows are
//! identified by `entity_type`/`entity_id` rather than an actor/target
//! user pair.

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::QueryBuilder;
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::begin_rls_transaction;
use crate::auth::AuthenticatedUser;
use crate::client_ops::audit_log;

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;
const PERMISSION: &str = "activity_logs.read";

#[derive(Debug, Deserialize)]
pub struct ActivityLogQuery {
    #[serde(default)]
    pub limit: Option<i64>,

    /// Keyset pagination: only rows with `id` less than this. `id` is a
    /// `uuidv7`, which sorts chronologically -- see `client_ops.audit_log`'s
    /// own migration comment -- so this compares directly the same way
    /// `auth_audit_logs`'s bigint `before_id` does.
    #[serde(default)]
    pub before_id: Option<Uuid>,

    #[serde(default)]
    pub event_type: Option<String>,

    #[serde(default)]
    pub entity_type: Option<String>,

    #[serde(default)]
    pub actor_user_id: Option<String>,
}

fn parse_comma_separated(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_actor_ids(raw: &str) -> Result<Vec<Uuid>, String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| Uuid::parse_str(value).map_err(|_| value.to_string()))
        .collect()
}

/// Shared with `client_ops_activity_logs_export`'s identical filter needs
/// -- a no-op on an empty list, `IN (...)` otherwise, same convention as
/// `auth_audit_logs::push_event_type_filter`.
pub(super) fn push_in_filter(builder: &mut QueryBuilder<sqlx::Postgres>, column: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }

    builder.push(format!(" AND {column} IN ("));
    {
        let mut separated = builder.separated(", ");
        for value in values {
            separated.push_bind(value.clone());
        }
    }
    builder.push(")");
}

pub(super) fn push_actor_filter(builder: &mut QueryBuilder<sqlx::Postgres>, column: &str, actor_ids: &[Uuid]) {
    if actor_ids.is_empty() {
        return;
    }

    builder.push(format!(" AND {column} IN ("));
    {
        let mut separated = builder.separated(", ");
        for actor_id in actor_ids {
            separated.push_bind(*actor_id);
        }
    }
    builder.push(")");
}

#[derive(Debug, Serialize)]
pub struct ActivityLogEntry {
    pub id: Uuid,
    pub event_type: String,
    pub actor_user_id: Option<Uuid>,
    pub entity_type: String,
    pub entity_id: Option<String>,
    pub metadata: serde_json::Value,
    pub before_state: Option<serde_json::Value>,
    pub after_state: Option<serde_json::Value>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ListActivityLogsResponse {
    pub entries: Vec<ActivityLogEntry>,
}

pub async fn list_activity_logs(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Query(query): Query<ActivityLogQuery>,
) -> Response {
    // Redundant with the RLS policy by design -- same reasoning as every
    // other permission-gated read in this app (see auth_invites.rs's
    // module doc).
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "list_activity_logs", None, None)
        .await
    {
        return response;
    }

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let actor_ids = match query.actor_user_id.as_deref().map(parse_actor_ids) {
        Some(Ok(ids)) => ids,
        Some(Err(bad_value)) => {
            return bad_request(
                "invalid_actor_user_id",
                format!("\"{bad_value}\" is not a valid UUID."),
            )
        }
        None => Vec::new(),
    };

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for activity log listing");
            return internal_error("Could not list activity logs");
        }
    };

    let mut builder: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
        "SELECT id, event_type, actor_user_id, entity_type, entity_id, metadata, before_state, \
         after_state, ip_address, user_agent, created_at \
         FROM client_ops.audit_log WHERE true",
    );

    if let Some(before_id) = query.before_id {
        builder.push(" AND id < ").push_bind(before_id);
    }
    if let Some(raw) = &query.event_type {
        push_in_filter(&mut builder, "event_type", &parse_comma_separated(raw));
    }
    if let Some(raw) = &query.entity_type {
        push_in_filter(&mut builder, "entity_type", &parse_comma_separated(raw));
    }
    push_actor_filter(&mut builder, "actor_user_id", &actor_ids);

    // Newest first, ordered by id -- same reasoning as auth_audit_logs:
    // id (uuidv7, time-ordered) is what before_id's keyset paging compares
    // against, so listing order and paging order must agree.
    builder.push(" ORDER BY id DESC LIMIT ").push_bind(limit);

    #[allow(clippy::type_complexity)]
    let rows: Result<
        Vec<(
            Uuid,
            String,
            Option<Uuid>,
            String,
            Option<String>,
            serde_json::Value,
            Option<serde_json::Value>,
            Option<serde_json::Value>,
            Option<sqlx::types::ipnetwork::IpNetwork>,
            Option<String>,
            DateTime<Utc>,
        )>,
        sqlx::Error,
    > = builder.build_query_as().fetch_all(&mut *tx).await;

    let rows = match rows {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "activity log listing query failed");
            return internal_error("Could not list activity logs");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit activity log listing transaction");
        return internal_error("Could not list activity logs");
    }

    let entries = rows
        .into_iter()
        .map(
            |(
                id,
                event_type,
                actor_user_id,
                entity_type,
                entity_id,
                metadata,
                before_state,
                after_state,
                ip_address,
                user_agent,
                created_at,
            )| ActivityLogEntry {
                id,
                event_type,
                actor_user_id,
                entity_type,
                entity_id,
                metadata,
                before_state,
                after_state,
                ip_address: ip_address.map(|ip| ip.to_string()),
                user_agent,
                created_at,
            },
        )
        .collect();

    Json(ListActivityLogsResponse { entries }).into_response()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ActivityEventTypesResponse {
    pub event_types: Vec<String>,
}

/// The canonical event-type list for the admin filter dropdown -- straight
/// from `audit_log::event::ALL`, same reasoning as
/// `auth_audit_logs::list_event_types`.
pub async fn list_event_types(State(state): State<AppState>, user: AuthenticatedUser) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "list_activity_log_event_types", None, None)
        .await
    {
        return response;
    }

    Json(ActivityEventTypesResponse {
        event_types: audit_log::event::ALL.iter().map(|s| s.to_string()).collect(),
    })
    .into_response()
}

pub(super) fn bad_request(error: &'static str, message: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(ApiErrorBody { error, message })).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{admin_user, empty_state, test_user};
    use axum::extract::Query as AxumQuery;

    #[tokio::test]
    async fn refuses_a_caller_without_the_permission_without_touching_the_database() {
        let response = list_activity_logs(
            State(empty_state()),
            test_user(),
            AxumQuery(ActivityLogQuery {
                limit: None,
                before_id: None,
                event_type: None,
                entity_type: None,
                actor_user_id: None,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn event_types_refuses_a_caller_without_the_permission() {
        let response = list_event_types(State(empty_state()), test_user()).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn event_types_returns_the_full_canonical_list_for_an_admin() {
        let response = list_event_types(State(empty_state()), admin_user()).await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body must be readable");
        let parsed: ActivityEventTypesResponse =
            serde_json::from_slice(&body).expect("response body must be valid JSON");

        let expected: Vec<String> = audit_log::event::ALL.iter().map(|s| s.to_string()).collect();
        assert_eq!(parsed.event_types, expected);
    }

    #[test]
    fn parse_comma_separated_trims_and_drops_empties() {
        assert_eq!(
            parse_comma_separated(" client_created , sync_completed ,, "),
            vec!["client_created".to_string(), "sync_completed".to_string()]
        );
    }

    #[test]
    fn parse_actor_ids_names_the_specific_value_that_failed_to_parse() {
        let a = Uuid::new_v4();

        let err = parse_actor_ids(&format!("{a},not-a-uuid")).unwrap_err();

        assert_eq!(err, "not-a-uuid");
    }

    #[tokio::test]
    async fn list_activity_logs_refuses_an_invalid_actor_id_without_touching_the_database() {
        let response = list_activity_logs(
            State(empty_state()),
            admin_user(),
            AxumQuery(ActivityLogQuery {
                limit: None,
                before_id: None,
                event_type: None,
                entity_type: None,
                actor_user_id: Some("not-a-uuid".to_string()),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// A valid actor id must reach the database -- the 500 here (against
    /// the unreachable test pool) is the success signal, same convention
    /// as `auth_audit_logs`'s own tests.
    #[tokio::test]
    async fn list_activity_logs_with_a_valid_actor_id_reaches_the_database() {
        let response = list_activity_logs(
            State(empty_state()),
            admin_user(),
            AxumQuery(ActivityLogQuery {
                limit: None,
                before_id: None,
                event_type: None,
                entity_type: None,
                actor_user_id: Some(Uuid::new_v4().to_string()),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
