//! Background ClickUp Copy jobs: the row that lets the page that started a
//! big bulk copy find out how it is going, and the two endpoints that
//! read it. Jobs are created and advanced by `bulk`; see the
//! `clickup_copy_jobs` migration for why this is the only state ClickUp
//! Copy keeps and why an abandoned job is detected on read.

use axum::{
    extract::{Json, Path, State},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::exec::ItemResult;
use super::lists::PERMISSION;
use crate::api::rls::try_response;
use crate::api::{internal_error, not_found, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};

/// A job that says it is running but has not advanced for this long was
/// cut off (the server restarted mid-copy). Generous: one row can wait a
/// whole rate-limit window for its slot.
const STALE_AFTER: &str = "5 minutes";

/// Jobs listed per company page.
const LIST_LIMIT: i64 = 10;

/// Records a new running job and returns its id.
pub(super) async fn create(
    db: &PgPool,
    user_id: Uuid,
    role_keys: &[String],
    company_id: Uuid,
    source_facility_id: Uuid,
    source_task_name: &str,
    total: usize,
) -> Result<Uuid, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO client_ops.clickup_copy_jobs
             (company_id, created_by, source_facility_id, source_task_name, total)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(company_id)
    .bind(user_id)
    .bind(source_facility_id)
    .bind(source_task_name)
    .bind(total as i32)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

/// Saves everything finished so far. Best-effort: a failed save only means
/// the page sees slightly old progress, and the next batch saves it all
/// again.
pub(super) async fn record_progress(
    db: &PgPool,
    user_id: Uuid,
    role_keys: &[String],
    job_id: Uuid,
    results: &[ItemResult],
) {
    let copied = results.iter().filter(|r| r.comment.ok).count() as i32;
    let failed = results.len() as i32 - copied;
    let json = serde_json::to_value(results).unwrap_or_default();

    let outcome: Result<(), sqlx::Error> = async {
        let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
        sqlx::query(
            "UPDATE client_ops.clickup_copy_jobs
                SET copied = $2, failed = $3, results = $4, updated_at = now()
              WHERE id = $1",
        )
        .bind(job_id)
        .bind(copied)
        .bind(failed)
        .bind(json)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }
    .await;

    if let Err(err) = outcome {
        tracing::warn!(error = %err, %job_id, "could not save ClickUp Copy job progress");
    }
}

/// Marks the job finished. `message` explains a whole-job failure.
pub(super) async fn finish(
    db: &PgPool,
    user_id: Uuid,
    role_keys: &[String],
    job_id: Uuid,
    status: &str,
    message: Option<&str>,
) {
    let outcome: Result<(), sqlx::Error> = async {
        let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
        sqlx::query(
            "UPDATE client_ops.clickup_copy_jobs
                SET status = $2, message = $3, updated_at = now(), finished_at = now()
              WHERE id = $1",
        )
        .bind(job_id)
        .bind(status)
        .bind(message)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }
    .await;

    if let Err(err) = outcome {
        tracing::error!(error = %err, %job_id, "could not mark a ClickUp Copy job finished");
    }
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct JobView {
    pub id: Uuid,
    /// `running`, `done`, `failed`, or `interrupted` (it said running but
    /// has gone quiet).
    pub status: String,
    pub source_task_name: String,
    pub total: i32,
    pub copied: i32,
    pub failed: i32,
    /// One entry per destination finished so far.
    pub results: serde_json::Value,
    pub message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

fn select_jobs(filter: &str) -> String {
    format!(
        "SELECT id,
                CASE WHEN status = 'running' AND updated_at < now() - interval '{STALE_AFTER}'
                     THEN 'interrupted' ELSE status END AS status,
                source_task_name, total, copied, failed, results, message, created_at, finished_at
           FROM client_ops.clickup_copy_jobs
          WHERE company_id = $1 {filter}
          ORDER BY created_at DESC
          LIMIT $2"
    )
}

/// `GET /clients/{company_id}/clickup/copy-jobs` -- the caller's recent
/// jobs for this company, newest first.
pub async fn list_copy_jobs(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
) -> Response {
    try_response!(
        user.require_permission(&state.db, PERMISSION, "clickup_copy_jobs", None, None)
            .await
    );

    let rows: Result<Vec<JobView>, sqlx::Error> = async {
        let mut tx = begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await?;
        let rows = sqlx::query_as(&select_jobs(""))
            .bind(company_id)
            .bind(LIST_LIMIT)
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(rows)
    }
    .await;

    match rows {
        Ok(jobs) => Json(jobs).into_response(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "ClickUp Copy job listing failed");
            internal_error("Could not load your ClickUp Copy jobs")
        }
    }
}

/// `GET /clients/{company_id}/clickup/copy-jobs/{job_id}`
pub async fn get_copy_job(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, job_id)): Path<(Uuid, Uuid)>,
) -> Response {
    try_response!(
        user.require_permission(&state.db, PERMISSION, "clickup_copy_jobs", None, None)
            .await
    );

    let row: Result<Option<JobView>, sqlx::Error> = async {
        let mut tx = begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await?;
        let row = sqlx::query_as(&select_jobs("AND id = $3"))
            .bind(company_id)
            .bind(1_i64)
            .bind(job_id)
            .fetch_optional(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(row)
    }
    .await;

    match row {
        Ok(Some(job)) => Json(job).into_response(),
        // Someone else's job and a missing one look the same (RLS).
        Ok(None) => not_found("not_found", "No such ClickUp Copy job.".to_string()),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "ClickUp Copy job lookup failed");
            internal_error("Could not load that ClickUp Copy job")
        }
    }
}
