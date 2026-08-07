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
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::api::{internal_error, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::infrastructure::csv_export::write_csv;
use crate::infrastructure::csv_safety::sanitize_cell;

#[derive(Debug, Serialize)]
pub struct UserSummary {
    pub id: Uuid,
    pub email: String,
    pub first_name: String,
    pub last_name: String,
    pub company: String,
    pub job_title: Option<String>,
    pub roles: Vec<String>,
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

/// The shared query behind both `list_users` and `export_users` -- one
/// place that reads `auth.list_users_for_admin()` and maps its columns
/// onto `UserSummary`, so the JSON listing and the CSV export can never
/// silently diverge on what a "user" is.
#[allow(clippy::type_complexity)]
async fn fetch_users_for_admin(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<Vec<UserSummary>, sqlx::Error> {
    let rows: Vec<(
        Uuid,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<Vec<String>>,
        String,
        DateTime<Utc>,
        i64,
        bool,
        Option<DateTime<Utc>>,
    )> = sqlx::query_as("SELECT * FROM auth.list_users_for_admin()")
        .fetch_all(&mut **tx)
        .await?;

    Ok(rows
        .into_iter()
        .map(
            |(
                id,
                email,
                first_name,
                last_name,
                company,
                job_title,
                roles,
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
                roles: roles.unwrap_or_default(),
                status,
                created_at,
                credential_count,
                totp_enrolled,
                last_seen_at,
            },
        )
        .collect())
}

pub async fn list_users(State(state): State<AppState>, admin: AuthenticatedUser) -> Response {
    // Redundant with the RLS-independent permission check inside
    // auth.list_users_for_admin itself, by the same design as
    // auth_invites.rs's own handlers: this produces a clean 403, and the
    // function's own check is what holds if this one is ever forgotten.
    if let Err(response) = admin
        .require_permission(&state.db, "users.manage", "list_users", None, None)
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, &admin.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for user listing");
            return internal_error("Could not list users");
        }
    };

    let users = match fetch_users_for_admin(&mut tx).await {
        Ok(users) => users,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "user listing query failed");
            return internal_error("Could not list users");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to commit user listing transaction");
        return internal_error("Could not list users");
    }

    Json(ListUsersResponse { users }).into_response()
}

const USER_CSV_HEADER: &[&str] = &[
    "ID",
    "Email",
    "First Name",
    "Last Name",
    "Company",
    "Job Title",
    "Roles",
    "Status",
    "Created At",
    "Credential Count",
    "TOTP Enrolled",
    "Last Seen At",
];

/// Maps one `UserSummary` to a CSV row. The destructure below has no `..`
/// on purpose: adding a field to `UserSummary` without adding it here is a
/// **compile error**, not a silently-missing column in every export from
/// then on. Stronger than a comment or a vault note, and it costs nothing
/// extra to write this way.
fn user_csv_record(user: &UserSummary) -> Vec<String> {
    let UserSummary {
        id,
        email,
        first_name,
        last_name,
        company,
        job_title,
        roles,
        status,
        created_at,
        credential_count,
        totp_enrolled,
        last_seen_at,
    } = user;

    vec![
        id.to_string(),
        sanitize_cell(email),
        sanitize_cell(first_name),
        sanitize_cell(last_name),
        sanitize_cell(company),
        job_title.as_deref().map(sanitize_cell).unwrap_or_default(),
        sanitize_cell(&roles.join("; ")),
        status.clone(),
        created_at.to_rfc3339(),
        credential_count.to_string(),
        totp_enrolled.to_string(),
        last_seen_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
    ]
}

/// Admin-only CSV export of the exact same user set `list_users` shows on
/// screen -- same query, same rows, just a different serialization. Not
/// audited as a successful action for the same reason `list_users` isn't:
/// this is a view of existing state, not a change to it. A refused export
/// (wrong role) is audited, matching every other admin-gated read here.
pub async fn export_users(State(state): State<AppState>, admin: AuthenticatedUser) -> Response {
    if let Err(response) = admin
        .require_permission(&state.db, "users.manage", "export_users", None, None)
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, &admin.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for user export");
            return internal_error("Could not export users");
        }
    };

    let users = match fetch_users_for_admin(&mut tx).await {
        Ok(users) => users,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "user export query failed");
            return internal_error("Could not export users");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to commit user export transaction");
        return internal_error("Could not export users");
    }

    let bytes = match write_csv(USER_CSV_HEADER, users.iter().map(user_csv_record)) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to build users CSV");
            return internal_error("Could not export users");
        }
    };

    let filename = format!("unitprep-users-{}.csv", Utc::now().format("%Y-%m-%d"));

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, "text/csv".parse().unwrap());
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{filename}\"")
            .parse()
            .unwrap(),
    );

    tracing::info!(
        admin_user_id = %admin.user_id,
        user_count = users.len(),
        "users CSV exported"
    );

    (headers, bytes).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, onboarding_manager_user};
    use axum::http::StatusCode;
    use uuid::Uuid;

    #[tokio::test]
    async fn list_users_refuses_insufficient_permission_without_touching_the_database() {
        let response = list_users(State(empty_state()), onboarding_manager_user()).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn export_users_refuses_insufficient_permission_without_touching_the_database() {
        let response = export_users(State(empty_state()), onboarding_manager_user()).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// The exhaustive destructure in `user_csv_record` is the real
    /// compile-time guard against a silently-missing column; this is a
    /// content-level backstop confirming the mapping produces what the
    /// header promises, so the two can't drift on ORDER even though both
    /// compile.
    #[test]
    fn user_csv_record_matches_the_header_column_for_column() {
        let user = UserSummary {
            id: Uuid::nil(),
            email: "ada@example.com".to_string(),
            first_name: "Ada".to_string(),
            last_name: "Lovelace".to_string(),
            company: "quikstor".to_string(),
            job_title: Some("Engineer".to_string()),
            roles: vec!["admin".to_string()],
            status: "active".to_string(),
            created_at: DateTime::<Utc>::MIN_UTC,
            credential_count: 2,
            totp_enrolled: true,
            last_seen_at: None,
        };

        let record = user_csv_record(&user);

        assert_eq!(record.len(), USER_CSV_HEADER.len());
        assert_eq!(record[0], Uuid::nil().to_string());
        assert_eq!(record[1], "ada@example.com");
        assert_eq!(record[2], "Ada");
        assert_eq!(record[3], "Lovelace");
        assert_eq!(record[4], "quikstor");
        assert_eq!(record[5], "Engineer");
        assert_eq!(record[6], "admin");
        assert_eq!(record[7], "active");
        assert_eq!(record[9], "2");
        assert_eq!(record[10], "true");
        assert_eq!(record[11], "");
    }

    /// A leading `=` in a name/email/company/job-title field must come out
    /// apostrophe-prefixed, same CSV-injection guard every other export in
    /// this codebase uses -- this export is not exempt just because the
    /// source data is admin-entered rather than facility-file-derived.
    #[test]
    fn user_csv_record_sanitizes_formula_like_fields() {
        let user = UserSummary {
            id: Uuid::nil(),
            email: "a@example.com".to_string(),
            first_name: "=cmd".to_string(),
            last_name: "Lovelace".to_string(),
            company: "quikstor".to_string(),
            job_title: Some("=SUM(A1)".to_string()),
            roles: vec!["admin".to_string()],
            status: "active".to_string(),
            created_at: DateTime::<Utc>::MIN_UTC,
            credential_count: 0,
            totp_enrolled: false,
            last_seen_at: None,
        };

        let record = user_csv_record(&user);

        assert_eq!(record[2], "'=cmd");
        assert_eq!(record[5], "'=SUM(A1)");
    }
}
