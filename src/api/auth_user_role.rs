//! Standalone admin action: change an already-enrolled user's role.
//! Distinct from role-at-invite-time (`auth_invites::CreateInviteRequest`)
//! -- that assigns a role to a brand-new account; this changes one that
//! already exists, which is why it goes through `auth.set_user_role`
//! (mirroring `auth.set_user_status`) rather than the INSERT path.

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
    audit_log, begin_rls_transaction, insufficient_role, AuthenticatedUser, Role,
};

#[derive(Debug, Deserialize)]
pub struct ChangeRoleRequest {
    pub role: String,
}

#[derive(Debug, Serialize)]
pub struct ChangeRoleResponse {
    pub user_id: Uuid,
    pub role: &'static str,
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

fn conflict(message: String) -> Response {
    (
        StatusCode::CONFLICT,
        Json(ApiErrorBody {
            error: "role_not_changeable",
            message,
        }),
    )
        .into_response()
}

pub async fn change_user_role(
    State(state): State<AppState>,
    admin: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(target_user_id): Path<Uuid>,
    Json(request): Json<ChangeRoleRequest>,
) -> Response {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let ip_address = Some(sqlx::types::ipnetwork::IpNetwork::from(addr.ip()));

    match admin.role {
        Role::Admin => {}
        Role::OnboardingManager => {
            audit_log::record(
                &state.db,
                audit_log::event::AUTHORIZATION_FAILURE,
                audit_log::Subjects::by(admin.user_id),
                user_agent,
                ip_address,
                audit_log::Change::none(),
                serde_json::json!({ "action": "change_user_role" }),
            )
            .await;
            return insufficient_role();
        }
    }

    let Some(new_role) = Role::from_db_text(&request.role.trim().to_ascii_lowercase()) else {
        return bad_request(
            "invalid_role",
            "role must be one of: admin, onboarding_manager".to_string(),
        );
    };

    // Never on your own row -- an admin locking themselves out of the
    // only role that can undo it is exactly the kind of mistake worth
    // ruling out structurally rather than trusting the UI's confirm
    // dialog to catch. Same stance as `deactivate_user`'s self-refusal.
    if target_user_id == admin.user_id {
        return bad_request(
            "cannot_change_own_role",
            "You cannot change your own role.".to_string(),
        );
    }

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, admin.role).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for role change");
            return internal_error("Could not change this user's role");
        }
    };

    let existing: Result<Option<(String,)>, sqlx::Error> =
        sqlx::query_as("SELECT role::text FROM auth.users WHERE id = $1 AND deleted_at IS NULL")
            .bind(target_user_id)
            .fetch_optional(&mut *tx)
            .await;

    let prior_role = match existing {
        Ok(Some((role,))) => role,
        Ok(None) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back after a missing user lookup");
            }
            return not_found();
        }
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "user lookup failed during role change");
            return internal_error("Could not change this user's role");
        }
    };

    if prior_role == new_role.as_db_text() {
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a no-op role change");
        }
        return conflict(format!("This user already has the {prior_role} role."));
    }

    // Refuses only when this specific change would zero out active admins
    // -- promoting someone, or demoting one admin while another remains
    // active, is unaffected. Doesn't stop two admins taking turns
    // demoting each other down to one and then that one demoting
    // themselves (already refused above, self-target) -- only the
    // single-request case where this admin's own action would be the
    // one that empties the role. Mirrors the reasoning in
    // deactivate_user's equivalent check.
    if prior_role == Role::Admin.as_db_text() && new_role != Role::Admin {
        let remaining_admins: Result<i64, sqlx::Error> = sqlx::query_scalar(
            "SELECT count(*) FROM auth.users
              WHERE role = 'admin'::auth.auth_role
                AND status = 'active'::auth.user_status
                AND deleted_at IS NULL
                AND id != $1",
        )
        .bind(target_user_id)
        .fetch_one(&mut *tx)
        .await;

        match remaining_admins {
            Ok(0) => {
                if let Err(err) = tx.rollback().await {
                    tracing::error!(error = %err, "failed to roll back a last-admin role change");
                }
                return conflict(
                    "This is the last active admin. Promote another user to admin before \
                     changing this one's role."
                        .to_string(),
                );
            }
            Ok(_) => {}
            Err(err) => {
                tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to count remaining admins during role change");
                if let Err(err) = tx.rollback().await {
                    tracing::error!(error = %err, "failed to roll back after a remaining-admins count failure");
                }
                return internal_error("Could not change this user's role");
            }
        }
    }

    let updated: Result<bool, sqlx::Error> =
        sqlx::query_scalar("SELECT auth.set_user_role($1, $2::auth.auth_role)")
            .bind(target_user_id)
            .bind(new_role.as_db_text())
            .fetch_one(&mut *tx)
            .await;

    let updated = match updated {
        Ok(updated) => updated,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "set_user_role failed during role change");
            return internal_error("Could not change this user's role");
        }
    };

    if !updated {
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a concurrently-changed role");
        }
        return conflict(
            "This user's role changed while this request was in progress. Check its current \
             state and try again."
                .to_string(),
        );
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "failed to commit role change");
        return internal_error("Could not change this user's role");
    }

    audit_log::record(
        &state.db,
        audit_log::event::ROLE_CHANGED,
        audit_log::Subjects::by(admin.user_id).about(target_user_id),
        user_agent,
        ip_address,
        audit_log::Change::from_to(
            serde_json::json!({ "role": prior_role }),
            serde_json::json!({ "role": new_role.as_db_text() }),
        ),
        serde_json::json!({}),
    )
    .await;

    tracing::info!(
        admin_user_id = %admin.user_id,
        target_user_id = %target_user_id,
        new_role = new_role.as_db_text(),
        "user role changed"
    );

    Json(ChangeRoleResponse {
        user_id: target_user_id,
        role: new_role.as_db_text(),
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::empty_state;

    fn admin() -> AuthenticatedUser {
        AuthenticatedUser {
            user_id: Uuid::new_v4(),
            role: Role::Admin,
            token_hash: vec![0u8; 32],
            elevated_until: None,
            requires_step_up: false,
        }
    }

    fn onboarding_manager() -> AuthenticatedUser {
        AuthenticatedUser {
            user_id: Uuid::new_v4(),
            role: Role::OnboardingManager,
            token_hash: vec![0u8; 32],
            elevated_until: None,
            requires_step_up: false,
        }
    }

    fn test_addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    #[tokio::test]
    async fn refuses_a_non_admin_role_without_touching_the_database() {
        let response = change_user_role(
            State(empty_state()),
            onboarding_manager(),
            test_addr(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            Json(ChangeRoleRequest {
                role: "admin".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn refuses_an_invalid_role_without_touching_the_database() {
        let response = change_user_role(
            State(empty_state()),
            admin(),
            test_addr(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            Json(ChangeRoleRequest {
                role: "superuser".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn refuses_to_change_own_role_without_touching_the_database() {
        let admin = admin();
        let self_id = admin.user_id;

        let response = change_user_role(
            State(empty_state()),
            admin,
            test_addr(),
            HeaderMap::new(),
            Path(self_id),
            Json(ChangeRoleRequest {
                role: "onboarding_manager".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
