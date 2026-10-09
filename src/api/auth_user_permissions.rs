//! Per-user permission grants -- the backend of the Users page's "Add
//! permissions" dialog. A user's effective permissions are the union of
//! what their roles carry and what is granted to them directly
//! (`auth.user_permissions`, merged in `auth.resolve_session`); this file
//! manages only the direct half.
//!
//! Only permissions flagged `directly_grantable` in `auth.permissions`
//! can be granted this way (a trigger enforces it regardless of caller),
//! so this endpoint can never be used to hand out something like
//! `users.manage_roles`. Gated on `user_permissions.manage`, never usable
//! on one's own account, and backed by RLS on the table itself -- the
//! same belt-and-braces shape as `auth_user_role`.
//!
//! Grants take effect on the target's very next request: sessions
//! resolve their permission set from the database on every call, so
//! there is nothing to invalidate.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Json, Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use uuid::Uuid;

use crate::api::rls::{begin_for, try_response};
use crate::api::{bad_request, internal_error, not_found, AppState};
use crate::auth::{audit_log, begin_rls_transaction, AuthenticatedUser};

const PERMISSION: &str = "user_permissions.manage";

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct GrantablePermission {
    pub key: String,
    pub label: String,
    pub description: Option<String>,
    /// Grouping for the dialog (e.g. "Integrations"). Data, not a
    /// frontend constant -- see `auth.permissions.category`.
    pub category: Option<String>,
    pub granted: bool,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct UserPermissionsResponse {
    pub user_id: Uuid,
    pub permissions: Vec<GrantablePermission>,
}

async fn target_exists(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target_user_id: Uuid,
) -> Result<bool, sqlx::Error> {
    // Through `auth.user_exists` rather than a direct SELECT: auth.users
    // is readable only by its owner and admins under RLS, so a department
    // manager's own lookup would silently find nothing.
    sqlx::query_scalar("SELECT auth.user_exists($1)")
        .bind(target_user_id)
        .fetch_one(&mut **tx)
        .await
}

/// Every directly-grantable permission with whether `target_user_id`
/// currently holds it directly.
async fn grantable_with_state(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target_user_id: Uuid,
) -> Result<Vec<GrantablePermission>, sqlx::Error> {
    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, String, Option<String>, Option<String>, bool)> = sqlx::query_as(
        "SELECT p.key, p.label, p.description, p.category,
                EXISTS (SELECT 1 FROM auth.user_permissions up
                         WHERE up.user_id = $1 AND up.permission_key = p.key)
           FROM auth.permissions p
          WHERE p.directly_grantable
          ORDER BY p.category NULLS LAST, p.label",
    )
    .bind(target_user_id)
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .map(
            |(key, label, description, category, granted)| GrantablePermission {
                key,
                label,
                description,
                category,
                granted,
            },
        )
        .collect())
}

fn held_keys(permissions: &[GrantablePermission]) -> Vec<String> {
    permissions
        .iter()
        .filter(|p| p.granted)
        .map(|p| p.key.clone())
        .collect()
}

pub async fn list_user_permissions(
    State(state): State<AppState>,
    admin: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(target_user_id): Path<Uuid>,
) -> Response {
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    try_response!(
        admin
            .require_permission(
                &state.db,
                PERMISSION,
                "list_user_permissions",
                user_agent,
                ip_address,
            )
            .await
    );

    let mut tx =
        try_response!(begin_for(&state, &admin, "Could not load this user's permissions").await);

    match target_exists(&mut tx, target_user_id).await {
        Ok(true) => {}
        Ok(false) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back after a missing user lookup");
            }
            return not_found("user_not_found", "No such user.".to_string());
        }
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "user lookup failed during permission listing");
            return internal_error("Could not load this user's permissions");
        }
    }

    let permissions = match grantable_with_state(&mut tx, target_user_id).await {
        Ok(permissions) => permissions,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "permission listing query failed");
            return internal_error("Could not load this user's permissions");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to commit permission listing");
        return internal_error("Could not load this user's permissions");
    }

    Json(UserPermissionsResponse {
        user_id: target_user_id,
        permissions,
    })
    .into_response()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Change {
    Grant,
    Revoke,
}

impl Change {
    fn action(self) -> &'static str {
        match self {
            Change::Grant => "grant_user_permission",
            Change::Revoke => "revoke_user_permission",
        }
    }

    fn failure_message(self) -> &'static str {
        match self {
            Change::Grant => "Could not grant this permission",
            Change::Revoke => "Could not revoke this permission",
        }
    }

    fn event(self) -> &'static str {
        match self {
            Change::Grant => audit_log::event::PERMISSION_GRANTED,
            Change::Revoke => audit_log::event::PERMISSION_REVOKED,
        }
    }
}

pub async fn grant_user_permission(
    state: State<AppState>,
    admin: AuthenticatedUser,
    addr: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    path: Path<(Uuid, String)>,
) -> Response {
    change_permission(Change::Grant, state, admin, addr, headers, path).await
}

pub async fn revoke_user_permission(
    state: State<AppState>,
    admin: AuthenticatedUser,
    addr: ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    path: Path<(Uuid, String)>,
) -> Response {
    change_permission(Change::Revoke, state, admin, addr, headers, path).await
}

/// Grant and revoke are the same flow with one differing statement, and
/// both are **idempotent**: granting what is already held, or revoking
/// what is not, succeeds and returns the unchanged state. The dialog is a
/// set of checkboxes, so "make it so" semantics are what the caller
/// means -- unlike roles, where a duplicate grant is worth a 409.
async fn change_permission(
    change: Change,
    State(state): State<AppState>,
    admin: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((target_user_id, permission_key)): Path<(Uuid, String)>,
) -> Response {
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    try_response!(
        admin
            .require_permission(
                &state.db,
                PERMISSION,
                change.action(),
                user_agent,
                ip_address,
            )
            .await
    );

    // Redundant with the RLS policies on auth.user_permissions by
    // design -- same reasoning as auth_user_role: a clean 400 here, the
    // database is what holds if this is ever forgotten.
    if target_user_id == admin.user_id {
        return bad_request(
            "cannot_change_own_permissions",
            "You cannot change your own permissions.".to_string(),
        );
    }

    let permission_key = permission_key.trim().to_ascii_lowercase();
    let failure = change.failure_message();

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, &admin.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for permission change");
            return internal_error(failure);
        }
    };

    match target_exists(&mut tx, target_user_id).await {
        Ok(true) => {}
        Ok(false) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back after a missing user lookup");
            }
            return not_found("user_not_found", "No such user.".to_string());
        }
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "user lookup failed during permission change");
            return internal_error(failure);
        }
    }

    let before = match grantable_with_state(&mut tx, target_user_id).await {
        Ok(permissions) => permissions,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to read prior permissions");
            return internal_error(failure);
        }
    };

    if !before.iter().any(|p| p.key == permission_key) {
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back after an ungrantable permission key");
        }
        return bad_request(
            "permission_not_grantable",
            format!("{permission_key} is not a permission that can be granted directly to a user."),
        );
    }

    let statement = match change {
        Change::Grant => {
            sqlx::query(
                "INSERT INTO auth.user_permissions (user_id, permission_key, granted_by)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (user_id, permission_key) DO NOTHING",
            )
            .bind(target_user_id)
            .bind(&permission_key)
            .bind(admin.user_id)
            .execute(&mut *tx)
            .await
        }
        Change::Revoke => {
            sqlx::query(
                "DELETE FROM auth.user_permissions WHERE user_id = $1 AND permission_key = $2",
            )
            .bind(target_user_id)
            .bind(&permission_key)
            .execute(&mut *tx)
            .await
        }
    };

    let changed_rows = match statement {
        Ok(result) => result.rows_affected(),
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "permission change statement failed");
            if let Err(rollback_err) = tx.rollback().await {
                tracing::error!(error = %rollback_err, "failed to roll back a failed permission change");
            }
            return internal_error(failure);
        }
    };

    let after = match grantable_with_state(&mut tx, target_user_id).await {
        Ok(permissions) => permissions,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to read permissions after change");
            return internal_error(failure);
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "failed to commit permission change");
        return internal_error(failure);
    }

    // Only a real transition is audited; an idempotent no-op is not an
    // event worth a row.
    if changed_rows > 0 {
        audit_log::record(
            &state.db,
            change.event(),
            audit_log::Subjects::by(admin.user_id).about(target_user_id),
            user_agent,
            ip_address,
            audit_log::Change::from_to(
                serde_json::json!({ "direct_permissions": held_keys(&before) }),
                serde_json::json!({ "direct_permissions": held_keys(&after) }),
            ),
            serde_json::json!({ "permission": permission_key }),
        )
        .await;

        tracing::info!(
            admin_user_id = %admin.user_id,
            target_user_id = %target_user_id,
            permission = %permission_key,
            "direct permission changed"
        );
    }

    Json(UserPermissionsResponse {
        user_id: target_user_id,
        permissions: after,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::{admin_user, empty_state, test_user};

    fn local_addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    #[tokio::test]
    async fn listing_refuses_a_caller_without_the_permission() {
        let response = list_user_permissions(
            State(empty_state()),
            test_user(),
            local_addr(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn granting_refuses_a_caller_without_the_permission() {
        let response = grant_user_permission(
            State(empty_state()),
            test_user(),
            local_addr(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), "integrations.clickup".to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn revoking_refuses_a_caller_without_the_permission() {
        let response = revoke_user_permission(
            State(empty_state()),
            test_user(),
            local_addr(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), "integrations.clickup".to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn an_admin_cannot_change_their_own_permissions() {
        let admin = admin_user();
        let own_id = admin.user_id;

        let response = grant_user_permission(
            State(empty_state()),
            admin,
            local_addr(),
            HeaderMap::new(),
            Path((own_id, "integrations.clickup".to_string())),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn an_admin_gets_past_the_gate_and_reaches_the_database() {
        let response = grant_user_permission(
            State(empty_state()),
            admin_user(),
            local_addr(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), "integrations.clickup".to_string())),
        )
        .await;
        // The disconnected test pool fails: 500, not 403.
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn held_keys_lists_only_granted_permissions() {
        let permissions = vec![
            GrantablePermission {
                key: "a".to_string(),
                label: "A".to_string(),
                description: None,
                category: None,
                granted: true,
            },
            GrantablePermission {
                key: "b".to_string(),
                label: "B".to_string(),
                description: None,
                category: None,
                granted: false,
            },
        ];
        assert_eq!(held_keys(&permissions), vec!["a".to_string()]);
    }
}
