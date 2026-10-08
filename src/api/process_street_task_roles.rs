//! Admin API for the Process Street "Task mapping" section: which PS task
//! names each role (see `clients::ps_task_roles`) resolves through.
//!
//! GET lists every known role with its current names and a coverage
//! readout -- how many linked facilities' synced Merchant Account runs
//! currently show a *visible* task matching the role -- so a template
//! rename that stops matching shows up as a drop in coverage instead of
//! silently. PUT replaces one role's whole name list (the page edits a
//! small list of chips; replace-all keeps the API one call and
//! idempotent). Both are admin-only (`integrations.manage`), like the
//! rest of the Process Street settings.

use axum::extract::ConnectInfo;
use axum::{
    extract::{Json, Path, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

use crate::api::rls::{begin_for, try_response};
use crate::api::{bad_request, internal_error, not_found, AppState};
use crate::auth::AuthenticatedUser;
use crate::clients::ps_task_roles::{self, KNOWN_ROLES};

const PERMISSION: &str = "integrations.manage";

/// Generous for a hand-maintained list of PS task names; keeps a bad
/// request from writing hundreds of rows.
const MAX_NAMES_PER_ROLE: usize = 20;
const MAX_NAME_LEN: usize = 200;

#[derive(Debug, Serialize)]
pub struct TaskRoleResponse {
    pub role: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub task_names: Vec<String>,
    /// Facilities with a synced Merchant Account checklist.
    pub facilities_total: i64,
    /// Of those, how many have a visible task matching one of
    /// `task_names`.
    pub facilities_matched: i64,
}

#[derive(Debug, Serialize)]
pub struct TaskRolesResponse {
    pub roles: Vec<TaskRoleResponse>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaskRoleRequest {
    pub task_names: Vec<String>,
}

#[derive(sqlx::FromRow)]
struct Coverage {
    facilities_total: i64,
    facilities_matched: i64,
}

async fn load_role(
    conn: &mut sqlx::PgConnection,
    role: &'static str,
    label: &'static str,
    description: &'static str,
) -> Result<TaskRoleResponse, sqlx::Error> {
    let task_names = ps_task_roles::load_task_names(conn, role).await?;

    let coverage: Coverage = sqlx::query_as(
        "SELECT COUNT(DISTINCT facility_id) AS facilities_total,
                COUNT(DISTINCT facility_id) FILTER (
                    WHERE NOT hidden AND lower(btrim(task_name)) IN (
                        SELECT lower(btrim(task_name))
                          FROM integrations.ps_task_role_name WHERE role = $1)
                ) AS facilities_matched
           FROM clients.ps_task_status
          WHERE workflow = 'merchant_account'",
    )
    .bind(role)
    .fetch_one(&mut *conn)
    .await?;

    Ok(TaskRoleResponse {
        role,
        label,
        description,
        task_names,
        facilities_total: coverage.facilities_total,
        facilities_matched: coverage.facilities_matched,
    })
}

pub async fn get_task_roles(State(state): State<AppState>, user: AuthenticatedUser) -> Response {
    try_response!(
        user.require_permission(
            &state.db,
            PERMISSION,
            "get_process_street_task_roles",
            None,
            None,
        )
        .await
    );

    let mut tx = try_response!(
        begin_for(
            &state,
            &user,
            "Could not load the Process Street task mapping"
        )
        .await
    );

    let mut roles = Vec::with_capacity(KNOWN_ROLES.len());
    for known in KNOWN_ROLES {
        match load_role(&mut tx, known.key, known.label, known.description).await {
            Ok(role) => roles.push(role),
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, role = known.key, "task role read failed");
                return internal_error("Could not load the Process Street task mapping");
            }
        }
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit task roles read");
        return internal_error("Could not load the Process Street task mapping");
    }

    Json(TaskRolesResponse { roles }).into_response()
}

/// Trims, drops blanks, and removes case-insensitive duplicates (first
/// spelling wins), preserving order.
fn normalize_names(raw: &[String]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    raw.iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .filter(|name| seen.insert(name.to_lowercase()))
        .map(str::to_string)
        .collect()
}

pub async fn update_task_role(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(role): Path<String>,
    Json(request): Json<UpdateTaskRoleRequest>,
) -> Response {
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    try_response!(
        user.require_permission(
            &state.db,
            PERMISSION,
            "update_process_street_task_role",
            user_agent,
            ip_address,
        )
        .await
    );

    let Some(known) = KNOWN_ROLES.iter().find(|known| known.key == role) else {
        return not_found(
            "unknown_task_role",
            format!("{role:?} is not a known task role."),
        );
    };

    let names = normalize_names(&request.task_names);
    if names.is_empty() {
        // An empty mapping would make every run read as "credentials
        // step not found" and uncap the Onboarding Summary walk.
        return bad_request(
            "task_names_required",
            "At least one task name is required.".to_string(),
        );
    }
    if names.len() > MAX_NAMES_PER_ROLE {
        return bad_request(
            "too_many_task_names",
            format!("At most {MAX_NAMES_PER_ROLE} task names are allowed per role."),
        );
    }
    if names.iter().any(|name| name.chars().count() > MAX_NAME_LEN) {
        return bad_request(
            "task_name_too_long",
            format!("Task names are limited to {MAX_NAME_LEN} characters."),
        );
    }

    let mut tx = try_response!(
        begin_for(
            &state,
            &user,
            "Could not update the Process Street task mapping"
        )
        .await
    );

    let result: Result<(), sqlx::Error> = async {
        // Keep rows for names that stay (original ids/ordering), remove
        // the ones that went, add the new ones.
        sqlx::query(
            "DELETE FROM integrations.ps_task_role_name
              WHERE role = $1 AND lower(btrim(task_name)) <> ALL($2)",
        )
        .bind(known.key)
        .bind(names.iter().map(|n| n.to_lowercase()).collect::<Vec<_>>())
        .execute(&mut *tx)
        .await?;

        for name in &names {
            sqlx::query(
                "INSERT INTO integrations.ps_task_role_name (role, task_name, created_by)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (role, lower(btrim(task_name))) DO NOTHING",
            )
            .bind(known.key)
            .bind(name)
            .bind(user.user_id)
            .execute(&mut *tx)
            .await?;
        }
        Ok(())
    }
    .await;

    if let Err(err) = result {
        tracing::error!(error = %err, user_id = %user.user_id, role = known.key, "task role update failed");
        if let Err(rollback_err) = tx.rollback().await {
            tracing::error!(error = %rollback_err, "failed to roll back a failed task role update");
        }
        return internal_error("Could not update the Process Street task mapping");
    }

    let response = match load_role(&mut tx, known.key, known.label, known.description).await {
        Ok(response) => response,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, role = known.key, "task role re-read failed");
            return internal_error("Could not update the Process Street task mapping");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit task role update");
        return internal_error("Could not update the Process Street task mapping");
    }

    crate::api::integration_settings_audit::record_settings_updated(
        &state.db,
        user.user_id,
        user_agent,
        ip_address,
        "process_street_task_roles",
        serde_json::json!({ "role": known.key, "task_names": names }),
    )
    .await;

    tracing::info!(user_id = %user.user_id, role = known.key, names = ?names, "Process Street task role updated");
    Json(response).into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[test]
    fn normalize_names_trims_drops_blanks_and_dedupes_case_insensitively() {
        let raw = vec![
            "  Document Credentials ".to_string(),
            "".to_string(),
            "document credentials".to_string(),
            "Add Credentials to QMS".to_string(),
        ];
        assert_eq!(
            normalize_names(&raw),
            vec!["Document Credentials", "Add Credentials to QMS"]
        );
    }

    #[tokio::test]
    async fn get_refuses_insufficient_permission() {
        let response = get_task_roles(State(empty_state()), test_user()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn update_refuses_insufficient_permission() {
        let response = update_task_role(
            State(empty_state()),
            test_user(),
            ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))),
            HeaderMap::new(),
            Path("qms_credentials".to_string()),
            Json(UpdateTaskRoleRequest {
                task_names: vec!["Document Credentials".to_string()],
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
