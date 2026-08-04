//! Admin-only audit log listing -- the backend half of the approved
//! frontend audit-log viewer. Read-only, same "a view of existing state is
//! not itself an event" reasoning as `auth_users.rs::list_users`: a
//! successful listing is not audited, a *refused* one is (wrong role).
//!
//! No SECURITY DEFINER function needed here, unlike `list_users_for_admin`
//! -- that one exists to bypass `webauthn_credentials`' owner-only RLS for
//! a cross-user join. `auth_audit_logs_select_admin_only` already grants
//! exactly the access this endpoint needs, so a plain SELECT inside
//! `begin_rls_transaction` is sufficient.

use axum::{
    extract::{Query, State},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::QueryBuilder;
use uuid::Uuid;

use crate::api::{internal_error, AppState};
use crate::auth::{audit_log, begin_rls_transaction, insufficient_role, AuthenticatedUser, Role};

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

#[derive(Debug, Deserialize)]
pub struct AuditLogQuery {
    #[serde(default)]
    pub limit: Option<i64>,

    /// Keyset pagination: only rows with `id` less than this. Preferred
    /// over an offset -- simpler against the table's own `id` ordering,
    /// and stable under concurrent inserts (a new row landing between two
    /// page requests cannot shift an offset-based page the way it would
    /// here).
    #[serde(default)]
    pub before_id: Option<i64>,

    #[serde(default)]
    pub event_type: Option<String>,

    /// Matches either `actor_user_id` or `target_user_id` -- an operator
    /// looking at one person's history wants both "what they did" and
    /// "what was done to them", not one or the other.
    #[serde(default)]
    pub user_id: Option<Uuid>,
}

#[derive(Debug, Serialize)]
pub struct AuditLogEntry {
    pub id: i64,
    pub event_type: String,
    pub actor_user_id: Option<Uuid>,
    pub target_user_id: Option<Uuid>,
    pub metadata: serde_json::Value,
    pub before_state: Option<serde_json::Value>,
    pub after_state: Option<serde_json::Value>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct ListAuditLogsResponse {
    pub entries: Vec<AuditLogEntry>,
}

pub async fn list_audit_logs(
    State(state): State<AppState>,
    admin: AuthenticatedUser,
    Query(query): Query<AuditLogQuery>,
) -> Response {
    // Redundant with the RLS policy by design -- see auth_invites.rs's
    // module doc for why both layers exist.
    match admin.role {
        Role::Admin => {}
        Role::OnboardingManager => {
            audit_log::record(
                &state.db,
                audit_log::event::AUTHORIZATION_FAILURE,
                audit_log::Subjects::by(admin.user_id),
                None,
                None,
                audit_log::Change::none(),
                serde_json::json!({ "action": "list_audit_logs" }),
            )
            .await;
            return insufficient_role();
        }
    }

    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, admin.role).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for audit log listing");
            return internal_error("Could not list audit logs");
        }
    };

    let mut builder: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
        "SELECT id, event_type, actor_user_id, target_user_id, metadata, before_state, \
         after_state, ip_address, user_agent, created_at \
         FROM auth.auth_audit_logs WHERE true",
    );

    if let Some(before_id) = query.before_id {
        builder.push(" AND id < ").push_bind(before_id);
    }
    if let Some(event_type) = &query.event_type {
        builder
            .push(" AND event_type = ")
            .push_bind(event_type.clone());
    }
    if let Some(user_id) = query.user_id {
        builder
            .push(" AND (actor_user_id = ")
            .push_bind(user_id)
            .push(" OR target_user_id = ")
            .push_bind(user_id)
            .push(")");
    }

    // Newest first -- the audit viewer's primary access pattern, matching
    // the index the schema already carries on created_at. Ordered by id
    // rather than created_at directly: id is the same monotonic order for
    // this identity-column table and is what before_id's keyset paging
    // above compares against, so the two must agree.
    builder.push(" ORDER BY id DESC LIMIT ").push_bind(limit);

    #[allow(clippy::type_complexity)]
    let rows: Result<
        Vec<(
            i64,
            String,
            Option<Uuid>,
            Option<Uuid>,
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
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "audit log listing query failed");
            return internal_error("Could not list audit logs");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to commit audit log listing transaction");
        return internal_error("Could not list audit logs");
    }

    let entries = rows
        .into_iter()
        .map(
            |(
                id,
                event_type,
                actor_user_id,
                target_user_id,
                metadata,
                before_state,
                after_state,
                ip_address,
                user_agent,
                created_at,
            )| AuditLogEntry {
                id,
                event_type,
                actor_user_id,
                target_user_id,
                metadata,
                before_state,
                after_state,
                ip_address: ip_address.map(|ip| ip.to_string()),
                user_agent,
                created_at,
            },
        )
        .collect();

    Json(ListAuditLogsResponse { entries }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::empty_state;
    use axum::extract::Query as AxumQuery;

    fn onboarding_manager() -> AuthenticatedUser {
        AuthenticatedUser {
            user_id: Uuid::new_v4(),
            role: Role::OnboardingManager,
            token_hash: vec![0u8; 32],
            elevated_until: None,
            requires_step_up: false,
        }
    }

    #[tokio::test]
    async fn refuses_a_non_admin_role_without_touching_the_database() {
        let response = list_audit_logs(
            State(empty_state()),
            onboarding_manager(),
            AxumQuery(AuditLogQuery {
                limit: None,
                before_id: None,
                event_type: None,
                user_id: None,
            }),
        )
        .await;

        assert_eq!(response.status(), axum::http::StatusCode::FORBIDDEN);
    }
}
