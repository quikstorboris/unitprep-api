//! The admin Users listing (Phase I item 8) -- read-only, distinct from
//! `auth_invites.rs`'s create/recover actions on purpose: this file's only
//! job is showing an admin what already exists, not changing anything.
//! A successful listing is not audited -- an audit trail records actions
//! taken, and looking at a list is not one; every action this list's UI
//! triggers (invite, reissue, recovery) already writes its own row in
//! `auth_invites.rs`. A *refused* listing (wrong role) is audited, though
//! -- see the `AUTHORIZATION_FAILURE` arm below -- since that is an
//! action someone took, not a view of existing state.

use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::api::{internal_error, AppState};
use crate::auth::{audit_log, begin_rls_transaction, insufficient_role, AuthenticatedUser, Role};

#[derive(Debug, Serialize)]
pub struct UserSummary {
    pub id: Uuid,
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    pub company: String,
    pub job_title: Option<String>,
    pub role: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub credential_count: i64,
    pub totp_enrolled: bool,
    /// The most recent `last_seen_at` across every session this user has
    /// ever had, or `None` if they have never had one (still `invited`,
    /// or every session has since expired past retention -- expired
    /// sessions aren't deleted today, so in practice this is only `None`
    /// for a not-yet-enrolled account). Backs the admin Users table's
    /// dormant-account indicator.
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct ListUsersResponse {
    pub users: Vec<UserSummary>,
}

pub async fn list_users(State(state): State<AppState>, admin: AuthenticatedUser) -> Response {
    // Redundant with the RLS-independent role check inside
    // auth.list_users_for_admin itself, by the same design as
    // auth_invites.rs's own handlers: this produces a clean 403, and the
    // function's own check is what holds if this one is ever forgotten.
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
                serde_json::json!({ "action": "list_users" }),
            )
            .await;
            return insufficient_role();
        }
    }

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, admin.role).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for user listing");
            return internal_error("Could not list users");
        }
    };

    #[allow(clippy::type_complexity)]
    let rows: Result<
        Vec<(
            Uuid,
            String,
            String,
            String,
            String,
            Option<String>,
            String,
            String,
            DateTime<Utc>,
            i64,
            bool,
            Option<DateTime<Utc>>,
        )>,
        sqlx::Error,
    > = sqlx::query_as("SELECT * FROM auth.list_users_for_admin()")
        .fetch_all(&mut *tx)
        .await;

    let rows = match rows {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "user listing query failed");
            return internal_error("Could not list users");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to commit user listing transaction");
        return internal_error("Could not list users");
    }

    let users = rows
        .into_iter()
        .map(
            |(
                id,
                email,
                first_name,
                last_name,
                company,
                job_title,
                role,
                status,
                created_at,
                credential_count,
                totp_enrolled,
                last_seen_at,
            )| UserSummary {
                id,
                email,
                first_name,
                last_name,
                company,
                job_title,
                role,
                status,
                created_at,
                credential_count,
                totp_enrolled,
                last_seen_at,
            },
        )
        .collect();

    Json(ListUsersResponse { users }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::empty_state;
    use axum::http::StatusCode;
    use uuid::Uuid;

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
    async fn list_users_refuses_a_non_admin_role_without_touching_the_database() {
        let response = list_users(State(empty_state()), onboarding_manager()).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
