//! Standalone admin actions on another user's account status: deactivate
//! and its counterpart, reactivate.
//!
//! `deactivate_user` was the approved backlog item, found via a real user
//! bug report ("disable user feature is not available in the FE") --
//! distinct from `auth_invites::recover_account`, which also passes a user
//! through `deactivated` but only as one step of reissuing a lost
//! credential. This is the action itself: an admin decided someone should
//! lose access, full stop, with no new invite issued.
//!
//! `reactivate_user` is the deliberately-deferred other half.
//! `recover_account` explicitly refuses a `deactivated` account with
//! "reactivating an account is a separate decision from recovering a lost
//! credential" -- this module is that separate decision, now built. A
//! deactivated account has already had every credential wiped by the
//! revoke-all-access-paths trigger fired at deactivation time, so flipping
//! `status` straight back to `active` would leave the account nominally
//! active with nothing able to sign into it. Instead this mirrors
//! `recover_account`'s destination: `deactivated -> invited`, plus a fresh
//! invite, so the account leaves this endpoint the same way any other
//! not-yet-enrolled account does.
//!
//! Both wrap the already-built `auth.set_user_status` primitive (see its
//! own migration doc) rather than writing a second status-flip path -- that
//! function was deliberately written generic for exactly this reuse.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use uuid::Uuid;

use crate::api::auth_invites::CreateInviteResponse;
use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{
    audit_log, begin_rls_transaction, generate_token, insufficient_role, AuthenticatedUser, Role,
};
use crate::bootstrap::invite_hours;

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

    let existing: Result<Option<(String, String)>, sqlx::Error> = sqlx::query_as(
        "SELECT status::text, role::text FROM auth.users WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(target_user_id)
    .fetch_optional(&mut *tx)
    .await;

    let (prior_status, prior_role) = match existing {
        Ok(Some(row)) => row,
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

    // Same reasoning as change_user_role's equivalent check: deactivating
    // the last active admin would leave the account with no one able to
    // undo it (short of the bootstrap-admin CLI's break-glass path).
    if prior_role == Role::Admin.as_db_text() && prior_status == "active" {
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
                    tracing::error!(error = %err, "failed to roll back a last-admin deactivation");
                }
                return conflict(
                    "This is the last active admin. Promote another user to admin before \
                     deactivating this one."
                        .to_string(),
                );
            }
            Ok(_) => {}
            Err(err) => {
                tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to count remaining admins during deactivation");
                if let Err(err) = tx.rollback().await {
                    tracing::error!(error = %err, "failed to roll back after a remaining-admins count failure");
                }
                return internal_error("Could not deactivate this user");
            }
        }
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

/// Reactivates a deactivated user: `deactivated -> invited`, plus a fresh
/// invite -- see the module doc for why this is the destination rather than
/// `active`.
pub async fn reactivate_user(
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

    // Same two-layer reasoning as deactivate_user above.
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
                serde_json::json!({ "action": "reactivate_user" }),
            )
            .await;
            return insufficient_role();
        }
    }

    let (raw_token, token_hash) = generate_token();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(invite_hours());

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, admin.role).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for user reactivation");
            return internal_error("Could not reactivate this user");
        }
    };

    let existing: Result<Option<String>, sqlx::Error> = sqlx::query_scalar(
        "SELECT status::text FROM auth.users WHERE id = $1 AND deleted_at IS NULL",
    )
    .bind(target_user_id)
    .fetch_optional(&mut *tx)
    .await;

    let prior_status = match existing {
        Ok(Some(status)) => status,
        Ok(None) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back after a missing user lookup");
            }
            return not_found();
        }
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "user lookup failed during reactivation");
            return internal_error("Could not reactivate this user");
        }
    };

    if prior_status != "deactivated" {
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a no-op reactivation");
        }
        return conflict(format!(
            "This user is not deactivated (current status: \"{prior_status}\")."
        ));
    }

    // No intermediate step through `deactivated` needed here, unlike
    // recover_account -- the revoke-all-access-paths trigger already fired
    // when this account *entered* `deactivated`, so this is a single flip.
    let updated: Result<bool, sqlx::Error> =
        sqlx::query_scalar("SELECT auth.set_user_status($1, 'invited'::auth.user_status)")
            .bind(target_user_id)
            .fetch_one(&mut *tx)
            .await;

    let updated = match updated {
        Ok(updated) => updated,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "set_user_status failed during reactivation");
            return internal_error("Could not reactivate this user");
        }
    };

    if !updated {
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a concurrently-changed reactivation");
        }
        return conflict(
            "This user's status changed while this request was in progress. Check its \
             current state and try again."
                .to_string(),
        );
    }

    // created_by defaults to app.current_user_id, set by
    // begin_rls_transaction above -- same reasoning as issue_invite.
    if let Err(err) = sqlx::query(
        "INSERT INTO auth.user_invites (user_id, token_hash, expires_at)
         VALUES ($1, $2, $3)",
    )
    .bind(target_user_id)
    .bind(&token_hash)
    .bind(expires_at)
    .execute(&mut *tx)
    .await
    {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "failed to insert invite during reactivation");
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back after a failed invite insert");
        }
        return internal_error("Could not reactivate this user");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, target_user_id = %target_user_id, "failed to commit user reactivation");
        return internal_error("Could not reactivate this user");
    }

    audit_log::record(
        &state.db,
        audit_log::event::USER_REACTIVATED,
        audit_log::Subjects::by(admin.user_id).about(target_user_id),
        user_agent,
        ip_address,
        audit_log::Change::from_to(
            serde_json::json!({ "status": prior_status }),
            serde_json::json!({ "status": "invited" }),
        ),
        serde_json::json!({ "expires_at": expires_at }),
    )
    .await;

    tracing::info!(
        admin_user_id = %admin.user_id,
        target_user_id = %target_user_id,
        "user reactivated"
    );

    (
        StatusCode::CREATED,
        Json(CreateInviteResponse {
            user_id: target_user_id,
            invite_token: raw_token,
            expires_at,
            reissued: true,
        }),
    )
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

    /// Same role-gating property as `refuses_a_non_admin_role_without_touching_the_database`
    /// above, on the reactivate endpoint.
    #[tokio::test]
    async fn reactivate_refuses_a_non_admin_role_without_touching_the_database() {
        let response = reactivate_user(
            State(empty_state()),
            onboarding_manager(),
            test_addr(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// An admin caller must reach the database -- the 500 here (against the
    /// unreachable test pool) is the success signal, same convention used
    /// throughout auth_invites.rs's own tests.
    #[tokio::test]
    async fn reactivate_with_a_valid_role_reaches_the_database() {
        let response = reactivate_user(
            State(empty_state()),
            admin(),
            test_addr(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
