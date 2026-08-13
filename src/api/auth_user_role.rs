//! Admin actions that grant or revoke a role on an already-enrolled user.
//! Distinct from role-at-invite-time (`auth_invites::CreateInviteRequest`)
//! -- that assigns a brand-new account's first role; these two endpoints
//! add or remove one role on an account that already exists, any number
//! of times, since a user can hold more than one role at once.
//!
//! Unlike the single-role `auth.set_user_role` primitive this replaces,
//! there is no SECURITY DEFINER function backing these -- `auth.user_roles`
//! is a normal table with normal grants, and its own RLS policies
//! (admin-only, never targeting the caller's own account) are the actual
//! enforcement. These handlers run a plain INSERT/DELETE inside
//! `begin_rls_transaction` and let the database refuse what it should
//! refuse.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{
    audit_log, begin_rls_transaction, remaining_active_admins_excluding, resolve_role_id,
    role_keys_for_user, AuthenticatedUser,
};

#[derive(Debug, Deserialize)]
pub struct GrantRoleRequest {
    pub role: String,
}

#[derive(Debug, Serialize)]
pub struct RoleChangeResponse {
    pub user_id: Uuid,
    /// The target's full, current role set after this change -- more
    /// useful to a caller than echoing back just the one role that
    /// changed, since the point of multi-role is that the whole set
    /// matters.
    pub roles: Vec<String>,
}

fn bad_request(error: &'static str, message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody { error, message }),
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ApiErrorBody {
            error: "user_not_found",
            message: "No such user.".to_string(),
        }),
    )
        .into_response()
}

fn conflict(error: &'static str, message: String) -> Response {
    (StatusCode::CONFLICT, Json(ApiErrorBody { error, message })).into_response()
}

/// Confirms the target account exists and isn't soft-deleted -- same
/// existence check every other admin-on-another-user action in this
/// codebase makes before touching anything.
async fn target_exists(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target_user_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM auth.users WHERE id = $1 AND deleted_at IS NULL)",
    )
    .bind(target_user_id)
    .fetch_one(&mut **tx)
    .await
}

pub async fn grant_role(
    State(state): State<AppState>,
    admin: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(target_user_id): Path<Uuid>,
    Json(request): Json<GrantRoleRequest>,
) -> Response {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let ip_address = Some(sqlx::types::ipnetwork::IpNetwork::from(addr.ip()));

    if let Err(response) = admin
        .require_permission(
            &state.db,
            "users.manage_roles",
            "grant_role",
            user_agent,
            ip_address,
        )
        .await
    {
        return response;
    }

    // Redundant with the RLS INSERT policy on auth.user_roles by design --
    // same reasoning as every other admin-on-self refusal in this
    // codebase: a clean 400 here, the database is what actually holds if
    // this is ever forgotten.
    if target_user_id == admin.user_id {
        return bad_request(
            "cannot_change_own_roles",
            "You cannot change your own roles.".to_string(),
        );
    }

    let role_key = request.role.trim().to_ascii_lowercase();

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, &admin.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for role grant");
            return internal_error("Could not grant this role");
        }
    };

    match target_exists(&mut tx, target_user_id).await {
        Ok(true) => {}
        Ok(false) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back after a missing user lookup");
            }
            return not_found();
        }
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "user lookup failed during role grant");
            return internal_error("Could not grant this role");
        }
    }

    let role_id: Uuid = match resolve_role_id(&mut tx, &role_key).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back after an unknown role key");
            }
            return bad_request("invalid_role", format!("No such role: {role_key}"));
        }
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "role lookup failed during grant");
            return internal_error("Could not grant this role");
        }
    };

    let before_roles = match role_keys_for_user(&mut tx, target_user_id).await {
        Ok(roles) => roles,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "failed to read prior roles during grant");
            return internal_error("Could not grant this role");
        }
    };

    if before_roles.iter().any(|held| held == &role_key) {
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a no-op role grant");
        }
        return conflict(
            "role_already_held",
            format!("This user already has the {role_key} role."),
        );
    }

    if let Err(err) = sqlx::query(
        "INSERT INTO auth.user_roles (user_id, role_id, granted_by) VALUES ($1, $2, $3)",
    )
    .bind(target_user_id)
    .bind(role_id)
    .bind(admin.user_id)
    .execute(&mut *tx)
    .await
    {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "role grant insert failed");
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a failed role grant");
        }
        return internal_error("Could not grant this role");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "failed to commit role grant");
        return internal_error("Could not grant this role");
    }

    let mut after_roles = before_roles.clone();
    after_roles.push(role_key.clone());
    after_roles.sort();

    audit_log::record(
        &state.db,
        audit_log::event::ROLE_GRANTED,
        audit_log::Subjects::by(admin.user_id).about(target_user_id),
        user_agent,
        ip_address,
        audit_log::Change::from_to(
            serde_json::json!({ "roles": before_roles }),
            serde_json::json!({ "roles": after_roles }),
        ),
        serde_json::json!({}),
    )
    .await;

    tracing::info!(
        admin_user_id = %admin.user_id,
        target_user_id = %target_user_id,
        role = %role_key,
        "role granted"
    );

    Json(RoleChangeResponse {
        user_id: target_user_id,
        roles: after_roles,
    })
    .into_response()
}

pub async fn revoke_role(
    State(state): State<AppState>,
    admin: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((target_user_id, role_key)): Path<(Uuid, String)>,
) -> Response {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let ip_address = Some(sqlx::types::ipnetwork::IpNetwork::from(addr.ip()));

    if let Err(response) = admin
        .require_permission(
            &state.db,
            "users.manage_roles",
            "revoke_role",
            user_agent,
            ip_address,
        )
        .await
    {
        return response;
    }

    if target_user_id == admin.user_id {
        return bad_request(
            "cannot_change_own_roles",
            "You cannot change your own roles.".to_string(),
        );
    }

    let role_key = role_key.trim().to_ascii_lowercase();

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, &admin.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for role revoke");
            return internal_error("Could not revoke this role");
        }
    };

    match target_exists(&mut tx, target_user_id).await {
        Ok(true) => {}
        Ok(false) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back after a missing user lookup");
            }
            return not_found();
        }
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "user lookup failed during role revoke");
            return internal_error("Could not revoke this role");
        }
    }

    let role_id: Uuid = match resolve_role_id(&mut tx, &role_key).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back after an unknown role key");
            }
            return bad_request("invalid_role", format!("No such role: {role_key}"));
        }
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "role lookup failed during revoke");
            return internal_error("Could not revoke this role");
        }
    };

    let before_roles = match role_keys_for_user(&mut tx, target_user_id).await {
        Ok(roles) => roles,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "failed to read prior roles during revoke");
            return internal_error("Could not revoke this role");
        }
    };

    if !before_roles.iter().any(|held| held == &role_key) {
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a no-op role revoke");
        }
        return conflict(
            "role_not_held",
            format!("This user does not have the {role_key} role."),
        );
    }

    // Refuses only when revoking THIS role from THIS user would zero out
    // active admins -- mirrors the last-admin reasoning this codebase
    // already applies to deactivation, re-scoped from a single role
    // column to a role_id count. Promoting someone, or revoking a role
    // from a user who isn't the last active admin, is unaffected.
    if role_key == "admin" {
        match remaining_active_admins_excluding(&mut tx, target_user_id).await {
            Ok(0) => {
                if let Err(err) = tx.rollback().await {
                    tracing::error!(error = %err, "failed to roll back a last-admin role revoke");
                }
                return conflict(
                    "last_active_admin",
                    "This is the last active admin. Promote another user to admin before \
                     revoking this one's admin role."
                        .to_string(),
                );
            }
            Ok(_) => {}
            Err(err) => {
                tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to count remaining admins during role revoke");
                if let Err(err) = tx.rollback().await {
                    tracing::error!(error = %err, "failed to roll back after a remaining-admins count failure");
                }
                return internal_error("Could not revoke this role");
            }
        }
    }

    if let Err(err) = sqlx::query("DELETE FROM auth.user_roles WHERE user_id = $1 AND role_id = $2")
        .bind(target_user_id)
        .bind(role_id)
        .execute(&mut *tx)
        .await
    {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "role revoke delete failed");
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a failed role revoke");
        }
        return internal_error("Could not revoke this role");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "failed to commit role revoke");
        return internal_error("Could not revoke this role");
    }

    let after_roles: Vec<String> = before_roles
        .iter()
        .filter(|held| *held != &role_key)
        .cloned()
        .collect();

    audit_log::record(
        &state.db,
        audit_log::event::ROLE_REVOKED,
        audit_log::Subjects::by(admin.user_id).about(target_user_id),
        user_agent,
        ip_address,
        audit_log::Change::from_to(
            serde_json::json!({ "roles": before_roles }),
            serde_json::json!({ "roles": after_roles }),
        ),
        serde_json::json!({}),
    )
    .await;

    tracing::info!(
        admin_user_id = %admin.user_id,
        target_user_id = %target_user_id,
        role = %role_key,
        "role revoked"
    );

    Json(RoleChangeResponse {
        user_id: target_user_id,
        roles: after_roles,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{admin_user, empty_state, onboarding_manager_user};

    fn test_addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    #[tokio::test]
    async fn grant_refuses_insufficient_permission_without_touching_the_database() {
        let response = grant_role(
            State(empty_state()),
            onboarding_manager_user(),
            test_addr(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            Json(GrantRoleRequest {
                role: "onboarding_manager".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn revoke_refuses_insufficient_permission_without_touching_the_database() {
        let response = revoke_role(
            State(empty_state()),
            onboarding_manager_user(),
            test_addr(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), "onboarding_manager".to_string())),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn grant_refuses_to_change_own_roles_without_touching_the_database() {
        let admin = admin_user();
        let self_id = admin.user_id;

        let response = grant_role(
            State(empty_state()),
            admin,
            test_addr(),
            HeaderMap::new(),
            Path(self_id),
            Json(GrantRoleRequest {
                role: "onboarding_manager".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn revoke_refuses_to_change_own_roles_without_touching_the_database() {
        let admin = admin_user();
        let self_id = admin.user_id;

        let response = revoke_role(
            State(empty_state()),
            admin,
            test_addr(),
            HeaderMap::new(),
            Path((self_id, "admin".to_string())),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn grant_with_sufficient_permission_reaches_the_database() {
        let response = grant_role(
            State(empty_state()),
            admin_user(),
            test_addr(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            Json(GrantRoleRequest {
                role: "onboarding_manager".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn revoke_with_sufficient_permission_reaches_the_database() {
        let response = revoke_role(
            State(empty_state()),
            admin_user(),
            test_addr(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), "onboarding_manager".to_string())),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
