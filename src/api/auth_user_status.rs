//! Standalone admin action: deactivate another user's account. Approved
//! backlog item, found via a real user bug report ("disable user feature
//! is not available in the FE") -- distinct from `auth_invites::recover_account`,
//! which also passes a user through `deactivated` but only as one step of
//! reissuing a lost credential. This is the action itself: an admin decided
//! someone should lose access, full stop, with no new invite issued.
//!
//! Wraps the already-built `auth.set_user_status` primitive (see its own
//! migration doc) rather than writing a second status-flip path -- that
//! function was deliberately written generic for exactly this reuse.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{
    audit_log, begin_rls_transaction, insufficient_role, AuthenticatedUser, Role,
};

#[derive(Debug, Serialize)]
pub struct DeactivateUserResponse {
    pub user_id: Uuid,
    pub status: &'static str,
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

/// Unlike the unauthenticated endpoints, an authenticated admin who can
/// already see the user list gets an explicit reason -- same stance as
/// `auth_invites.rs`'s own `conflict`.
fn conflict(message: String) -> Response {
    (
        StatusCode::CONFLICT,
        Json(ApiErrorBody {
            error: "user_not_deactivatable",
            message,
        }),
    )
        .into_response()
}

pub async fn deactivate_user(
    State(state): State<AppState>,
    admin: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(target_user_id): Path<Uuid>,
) -> Response {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let ip_address = Some(sqlx::types::ipnetwork::IpNetwork::from(addr.ip()));

    // Redundant with the RLS policy / set_user_status's own SECURITY
    // DEFINER check by design -- see auth_invites.rs's module doc for why
    // both layers exist.
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
                serde_json::json!({ "action": "deactivate_user" }),
            )
            .await;
            return insufficient_role();
        }
    }

    if target_user_id == admin.user_id {
        return bad_request(
            "cannot_deactivate_self",
            "You cannot deactivate your own account.".to_string(),
        );
    }

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, admin.role).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for user deactivation");
            return internal_error("Could not deactivate this user");
        }
    };

    let existing: Result<Option<(String,)>, sqlx::Error> = sqlx::query_as(
        "SELECT status::text FROM auth.users WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(target_user_id)
    .fetch_optional(&mut *tx)
    .await;

    let prior_status = match existing {
        Ok(Some((status,))) => status,
        Ok(None) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back after a missing user lookup");
            }
            return not_found();
        }
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "user lookup failed during deactivation");
            return internal_error("Could not deactivate this user");
        }
    };

    if prior_status == "deactivated" {
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a no-op deactivation");
        }
        return conflict("This user is already deactivated.".to_string());
    }

    let updated: Result<bool, sqlx::Error> =
        sqlx::query_scalar("SELECT auth.set_user_status($1, 'deactivated'::auth.user_status)")
            .bind(target_user_id)
            .fetch_one(&mut *tx)
            .await;

    let updated = match updated {
        Ok(updated) => updated,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "set_user_status failed during deactivation");
            return internal_error("Could not deactivate this user");
        }
    };

    if !updated {
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a concurrently-changed deactivation");
        }
        return conflict(
            "This user's status changed while this request was in progress. Check its \
             current state and try again."
                .to_string(),
        );
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "failed to commit user deactivation");
        return internal_error("Could not deactivate this user");
    }

    audit_log::record(
        &state.db,
        audit_log::event::USER_DEACTIVATED,
        audit_log::Subjects::by(admin.user_id).about(target_user_id),
        user_agent,
        ip_address,
        audit_log::Change::from_to(
            serde_json::json!({ "status": prior_status }),
            serde_json::json!({ "status": "deactivated" }),
        ),
        serde_json::json!({}),
    )
    .await;

    tracing::info!(
        admin_user_id = %admin.user_id,
        target_user_id = %target_user_id,
        "user deactivated"
    );

    Json(DeactivateUserResponse {
        user_id: target_user_id,
        status: "deactivated",
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
        let response = deactivate_user(
            State(empty_state()),
            onboarding_manager(),
            test_addr(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn refuses_to_deactivate_self_without_touching_the_database() {
        let admin = admin();
        let self_id = admin.user_id;

        let response = deactivate_user(
            State(empty_state()),
            admin,
            test_addr(),
            HeaderMap::new(),
            Path(self_id),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
