//! Company page's Onboarding Summary tab -- one row per facility, two
//! signals rolled up from data each facility's own tabs already track
//! individually: the next outstanding step in its Merchant Account
//! (Elavon) Process Street workflow, and how many Dedup checks
//! (`client_ops.tool_runs`) it has on record. Read-only, same "any
//! authenticated caller" posture as `clients_detail` -- the genuinely
//! sensitive join (`clients.facility_merchant_accounts`/
//! `clients.ps_task_status`, both `onboarding_manager`/
//! `department_manager`/`developer`-only at the RLS level) just comes
//! back empty for a caller without that role, same as `clients_detail`'s
//! own `fetch_elavon_active`.
//!
//! **Step ordering, since Process Street's own task API never returns
//! one**: `clients.ps_task_status.id` (`BIGSERIAL`) is assigned the
//! first time a task is ever seen for a facility, in the order
//! `repository::upsert_task_status` iterates `get_run_tasks`'s response
//! -- itself PS's own checklist order, the only ordering signal that
//! exists anywhere in this pipeline (`process_street::Task` carries no
//! position/index field at all). A later resync only updates existing
//! rows in place (`ON CONFLICT ... DO UPDATE`), never reassigning `id`,
//! so `ORDER BY id ASC` stays a stable proxy for step order across
//! resyncs, not just at first sync.
//!
//! **Capped at "Add Credentials to QMS", not the run's last task**
//! (2026-09-23, Boris, against a real run): a real Merchant Account run
//! carries far more tasks than the customer-facing approval sequence --
//! confirmed live on the Katy-Flewellen facility's own run, 27 tasks
//! deep, with a long tail after QMS credentials get added ("Twilio
//! Information", "Step for Trojan"/"Step for Absolute"/"Step for
//! Menards", "Add Credentials to YouTrack Card", "Request IP
//! Whitelisting", "Update UDF in Zoho CRM", ...) that are PS-internal/
//! per-vendor follow-ups, not steps an onboarding coordinator is
//! tracking through this tab. Walking the full list unmodified surfaced
//! whichever of those happened to be first incomplete as if it were the
//! next real blocker. `qms_task` finds that one named task (if this
//! run has it at all -- older/differently-templated runs might not,
//! in which case the walk below is intentionally left uncapped rather
//! than guessing) and `next_step` only ever considers tasks at or
//! before it.

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use uuid::Uuid;

use crate::api::{internal_error, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct FacilityOnboardingSummary {
    pub facility_id: Uuid,
    pub facility_name: String,
    /// Whether this facility has a linked Merchant Account run at all --
    /// distinguishes "not started" from "fully complete" when
    /// `elavon_next_step` is `None` for both.
    pub elavon_linked: bool,
    /// The first still-incomplete task's name at or before "Add
    /// Credentials to QMS" in PS checklist order, or `None` when either
    /// nothing is linked yet or every task up to and including that one
    /// is already `Completed` (later, PS-internal-only tasks are not
    /// consulted -- see this module's own doc comment).
    pub elavon_next_step: Option<String>,
    /// Whether `elavon_next_step` *is* "Add Credentials to QMS" itself
    /// -- lets the frontend show a reminder that this one is a manual
    /// action nobody but a person adding it to QMS ever completes, not
    /// something that resolves itself once the PS application is
    /// approved.
    pub elavon_awaiting_credentials: bool,
    /// `client_ops.tool_runs` rows for this facility (`tool = 'dedup'`)
    /// -- a row only exists once a check has actually succeeded, so this
    /// is already a completed-check count, not something that needs
    /// filtering further.
    pub duplicate_checks_completed: i64,
}

#[derive(Debug, Serialize)]
pub struct OnboardingSummaryResponse {
    pub facilities: Vec<FacilityOnboardingSummary>,
}

pub async fn get_onboarding_summary(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for onboarding summary");
            return internal_error("Could not load this company's Onboarding Summary");
        }
    };

    let rows: Result<Vec<FacilityOnboardingSummary>, sqlx::Error> = sqlx::query_as(
        "SELECT f.id AS facility_id, f.name AS facility_name,
                (fma.facility_id IS NOT NULL) AS elavon_linked,
                next_step.task_name AS elavon_next_step,
                (next_step.id IS NOT NULL AND next_step.id = qms_task.id) AS elavon_awaiting_credentials,
                COALESCE(dedup_counts.run_count, 0) AS duplicate_checks_completed
           FROM clients.facilities f
           LEFT JOIN clients.facility_merchant_accounts fma ON fma.facility_id = f.id
           LEFT JOIN LATERAL (
             SELECT id
               FROM clients.ps_task_status
              WHERE facility_id = f.id AND workflow = 'merchant_account'
                AND trim(task_name) ILIKE 'Add Credentials to QMS'
              ORDER BY id ASC
              LIMIT 1
           ) qms_task ON true
           LEFT JOIN LATERAL (
             SELECT id, task_name
               FROM clients.ps_task_status
              WHERE facility_id = f.id AND workflow = 'merchant_account' AND status <> 'Completed'
                AND (qms_task.id IS NULL OR id <= qms_task.id)
              ORDER BY id ASC
              LIMIT 1
           ) next_step ON true
           LEFT JOIN LATERAL (
             SELECT COUNT(*) AS run_count
               FROM client_ops.tool_runs
              WHERE facility_id = f.id AND tool = 'dedup'
           ) dedup_counts ON true
          WHERE f.company_id = $1
          ORDER BY f.name",
    )
    .bind(company_id)
    .fetch_all(&mut *tx)
    .await;

    let facilities = match rows {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "onboarding summary query failed");
            return internal_error("Could not load this company's Onboarding Summary");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit onboarding summary transaction");
        return internal_error("Could not load this company's Onboarding Summary");
    }

    Json(OnboardingSummaryResponse { facilities }).into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn get_onboarding_summary_reaches_the_database() {
        let response =
            get_onboarding_summary(State(empty_state()), test_user(), Path(Uuid::new_v4())).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
