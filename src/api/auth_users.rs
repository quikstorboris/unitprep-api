//! The admin Users listing (Phase I item 8) -- read-only, distinct from
//! `auth_invites.rs`'s create/recover actions on purpose: this file's only
//! job is showing an admin what already exists, not changing anything.
//! Not audited -- an audit trail records actions taken, and looking at a
//! list is not one; every action this list's UI triggers (invite,
//! reissue, recovery) already writes its own row in `auth_invites.rs`.

use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::api::{internal_error, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser, Role};

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
            },
        )
        .collect();

    Json(ListUsersResponse { users }).into_response()
}
