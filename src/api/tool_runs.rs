//! Onboarding Work tab's backend -- lists a facility's past tool runs
//! (`client_ops.tool_runs`, written by `dedup.rs`'s `check`/
//! `import_from_dropbox`/`export`/`export_to_dropbox`) and serves a run's
//! stored output file. Read-only: this module never writes to
//! `tool_runs` itself, see `client_ops::tool_runs` for the writers.
//!
//! Any authenticated caller -- same posture as every other read-only
//! facility tab (`clients_facility_people`, `clients_elavon`'s GET,
//! etc.): RLS's own `tool_runs_select_authenticated` policy is the real
//! backstop, not a permission check here.

use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::dedup::file_response;
use crate::api::{internal_error, not_found, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};

const DEFAULT_LIMIT: i64 = 20;
const MAX_LIMIT: i64 = 100;

#[derive(Debug, Deserialize)]
pub struct ListToolRunsQuery {
    pub tool: String,
    #[serde(default)]
    pub before_id: Option<Uuid>,
    #[serde(default)]
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ToolRunSummary {
    pub id: Uuid,
    /// This run's 1-based position among this facility's own runs of
    /// this same tool, oldest first ("1st Duplicate Check", "2nd
    /// Duplicate Check", ...) -- computed server-side over the whole
    /// partition (not just the current page), so it stays correct
    /// across pagination.
    pub sequence_number: i64,
    pub tool: String,
    pub actor_user_id: Option<Uuid>,
    pub actor_first_name: Option<String>,
    pub actor_last_name: Option<String>,
    pub actor_email: Option<String>,
    pub source_file_name: String,
    pub source_dropbox_path: Option<String>,
    /// Whether this run's original source file was captured into the DB
    /// -- always true for a run created after the 2026-09-10 fix (both
    /// `check` and `import_from_dropbox` always attach it), false only
    /// for a handful of rows that predate that column.
    pub has_source_file: bool,
    pub report_summary: serde_json::Value,
    pub output_kind: String,
    pub output_dropbox_path: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct ListToolRunsResponse {
    pub runs: Vec<ToolRunSummary>,
}

#[allow(clippy::type_complexity)]
type ToolRunRow = (
    Uuid,
    i64,
    String,
    Option<Uuid>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    bool,
    serde_json::Value,
    String,
    Option<String>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);

fn row_to_summary(row: ToolRunRow) -> ToolRunSummary {
    let (
        id,
        sequence_number,
        tool,
        actor_user_id,
        actor_first_name,
        actor_last_name,
        actor_email,
        source_file_name,
        source_dropbox_path,
        has_source_file,
        report_summary,
        output_kind,
        output_dropbox_path,
        created_at,
        completed_at,
    ) = row;

    ToolRunSummary {
        id,
        sequence_number,
        tool,
        actor_user_id,
        actor_first_name,
        actor_last_name,
        actor_email,
        source_file_name,
        source_dropbox_path,
        has_source_file,
        report_summary,
        output_kind,
        output_dropbox_path,
        created_at,
        completed_at,
    }
}

/// Verifies `facility_id` actually belongs to `company_id` -- same
/// inline check `clients_facility_people`'s own handlers each repeat;
/// there's no shared helper for it in this codebase today.
async fn facility_belongs_to_company(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    facility_id: Uuid,
    company_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM clients.facilities WHERE id = $1 AND company_id = $2")
            .bind(facility_id)
            .bind(company_id)
            .fetch_optional(&mut **tx)
            .await?;

    Ok(row.is_some())
}

pub async fn list_facility_tool_runs(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<ListToolRunsQuery>,
) -> Response {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for tool run listing");
            return internal_error("Could not load this facility's Onboarding Work tab");
        }
    };

    match facility_belongs_to_company(&mut tx, facility_id, company_id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = tx.commit().await;
            return not_found("not_found", "No such facility.".to_string());
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for tool runs failed");
            return internal_error("Could not load this facility's Onboarding Work tab");
        }
    }

    // `sequence_number` is windowed over the whole facility+tool
    // partition, not just this page, so it stays correct regardless of
    // where `before_id` starts. `output_bytes` itself is never selected
    // here -- keeps list pages cheap regardless of stored blob size.
    let rows: Result<Vec<ToolRunRow>, sqlx::Error> = sqlx::query_as(
        "SELECT sub.id, sub.sequence_number, sub.tool, sub.actor_user_id,
                u.first_name AS actor_first_name, u.last_name AS actor_last_name, u.email::text AS actor_email,
                sub.source_file_name, sub.source_dropbox_path, sub.has_source_file, sub.report_summary,
                CASE WHEN sub.has_output_bytes THEN 'download'
                     WHEN sub.output_dropbox_path IS NOT NULL THEN 'dropbox'
                     ELSE 'none' END AS output_kind,
                sub.output_dropbox_path, sub.created_at, sub.completed_at
           FROM (
             SELECT id, tool, actor_user_id, source_file_name, source_dropbox_path,
                    (source_bytes IS NOT NULL) AS has_source_file,
                    report_summary, (output_bytes IS NOT NULL) AS has_output_bytes,
                    output_dropbox_path, created_at, completed_at,
                    ROW_NUMBER() OVER (PARTITION BY facility_id, tool ORDER BY created_at ASC) AS sequence_number
               FROM client_ops.tool_runs
              WHERE facility_id = $1 AND tool = $2
           ) sub
           LEFT JOIN auth.users u ON u.id = sub.actor_user_id
          WHERE ($3::uuid IS NULL OR sub.id < $3)
          ORDER BY sub.id DESC
          LIMIT $4",
    )
    .bind(facility_id)
    .bind(&query.tool)
    .bind(query.before_id)
    .bind(limit)
    .fetch_all(&mut *tx)
    .await;

    let rows = match rows {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "tool run listing query failed");
            return internal_error("Could not load this facility's Onboarding Work tab");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit tool run listing transaction");
        return internal_error("Could not load this facility's Onboarding Work tab");
    }

    Json(ListToolRunsResponse {
        runs: rows.into_iter().map(row_to_summary).collect(),
    })
    .into_response()
}

pub async fn download_tool_run_output(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id, run_id)): Path<(Uuid, Uuid, Uuid)>,
) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for tool run output download");
            return internal_error("Could not download this run's output");
        }
    };

    match facility_belongs_to_company(&mut tx, facility_id, company_id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = tx.commit().await;
            return not_found("not_found", "No such facility.".to_string());
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for tool run output failed");
            return internal_error("Could not download this run's output");
        }
    }

    #[allow(clippy::type_complexity)]
    let row: Result<Option<(Option<Vec<u8>>, Option<String>, Option<String>)>, sqlx::Error> =
        sqlx::query_as(
            "SELECT output_bytes, output_content_type, output_file_name
               FROM client_ops.tool_runs
              WHERE id = $1 AND facility_id = $2",
        )
        .bind(run_id)
        .bind(facility_id)
        .fetch_optional(&mut *tx)
        .await;

    let row = match row {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "tool run output lookup failed");
            return internal_error("Could not download this run's output");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit tool run output transaction");
        return internal_error("Could not download this run's output");
    }

    match row {
        Some((Some(bytes), Some(content_type), Some(file_name))) => {
            file_response(bytes, &content_type, &file_name)
        }
        _ => not_found(
            "tool_run_output_not_found",
            "No stored output for this run.".to_string(),
        ),
    }
}

/// Serves this run's original source file straight from the DB --
/// deliberately independent of Dropbox, even when `source_dropbox_path`
/// is also set: that path can be moved, renamed, or deleted out from
/// under the run after the fact, so this stored copy is the one
/// reference that always still works.
pub async fn download_tool_run_source(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id, run_id)): Path<(Uuid, Uuid, Uuid)>,
) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for tool run source download");
            return internal_error("Could not download this run's source file");
        }
    };

    match facility_belongs_to_company(&mut tx, facility_id, company_id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = tx.commit().await;
            return not_found("not_found", "No such facility.".to_string());
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for tool run source failed");
            return internal_error("Could not download this run's source file");
        }
    }

    #[allow(clippy::type_complexity)]
    let row: Result<Option<(Option<Vec<u8>>, Option<String>, String)>, sqlx::Error> = sqlx::query_as(
        "SELECT source_bytes, source_content_type, source_file_name
           FROM client_ops.tool_runs
          WHERE id = $1 AND facility_id = $2",
    )
    .bind(run_id)
    .bind(facility_id)
    .fetch_optional(&mut *tx)
    .await;

    let row = match row {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "tool run source lookup failed");
            return internal_error("Could not download this run's source file");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit tool run source transaction");
        return internal_error("Could not download this run's source file");
    }

    match row {
        Some((Some(bytes), Some(content_type), file_name)) => file_response(bytes, &content_type, &file_name),
        _ => not_found(
            "tool_run_source_not_found",
            "No stored source file for this run.".to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn list_facility_tool_runs_reaches_the_database() {
        let response = list_facility_tool_runs(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Query(ListToolRunsQuery { tool: "dedup".to_string(), before_id: None, limit: None }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn download_tool_run_output_reaches_the_database() {
        let response = download_tool_run_output(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn download_tool_run_source_reaches_the_database() {
        let response = download_tool_run_source(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
