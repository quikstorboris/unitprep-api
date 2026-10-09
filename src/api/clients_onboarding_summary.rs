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
//! **Capped at the QMS-credentials step, not the run's last task**
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
//!
//! **Which task that is, and hidden tasks** (2026-10-06): the cap task is
//! whichever *visible* task matches one of the names mapped to
//! `ps_task_roles::QMS_CREDENTIALS_ROLE` (`integrations.ps_task_role_name`,
//! edited on the Process Street settings page) -- not a hardcoded string.
//! A 2026-10 template change renamed the step to "Document Credentials"
//! and left the old "Add Credentials to QMS" task in the run but hidden
//! by PS conditional logic; `ps_task_status.hidden` mirrors PS's flag
//! and every read here ignores hidden tasks, both as the cap and as
//! candidate "next steps", since a task the coordinator can't see is not
//! one anybody is waiting on.

use axum::{
    extract::{Path, State},
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use uuid::Uuid;

use crate::api::rls::{begin_for, try_response};
use crate::api::{internal_error, AppState};
use crate::auth::AuthenticatedUser;

#[derive(Debug, Serialize, sqlx::FromRow, ts_rs::TS)]
#[ts(export)]
pub struct FacilityOnboardingSummary {
    pub facility_id: Uuid,
    pub facility_name: String,
    /// Whether this facility has a linked Merchant Account run at all --
    /// distinguishes "not started" from "fully complete" when
    /// `elavon_next_step` is `None` for both.
    pub elavon_linked: bool,
    /// The first still-incomplete visible task's name at or before the
    /// QMS-credentials step (see this module's doc) in PS checklist order, or `None` when either
    /// nothing is linked yet or every task up to and including that one
    /// is already `Completed` (later, PS-internal-only tasks are not
    /// consulted -- see this module's own doc comment).
    pub elavon_next_step: Option<String>,
    /// Whether `elavon_next_step` *is* the QMS-credentials step itself
    /// -- lets the frontend show a reminder that this one is a manual
    /// action nobody but a person adding it to QMS ever completes, not
    /// something that resolves itself once the PS application is
    /// approved.
    pub elavon_awaiting_credentials: bool,
    /// Whether the QMS-credentials step (every visible task mapped to
    /// `ps_task_roles::QMS_CREDENTIALS_ROLE`) is Completed -- the sole
    /// definition of "Complete" here (2026-10-06, Boris), independent
    /// of `elavon_next_step`: an earlier step left open (e.g.
    /// "Application Signed & Submitted to Elavon") does not stop a
    /// facility whose credentials step is done from reading Complete.
    /// Same rule as `ps_task_roles::role_is_satisfied`.
    pub elavon_complete: bool,
    /// `client_ops.tool_runs` rows for this facility (`tool = 'dedup'`)
    /// -- a row only exists once a check has actually succeeded, so this
    /// is already a completed-check count, not something that needs
    /// filtering further.
    #[ts(type = "number")]
    pub duplicate_checks_completed: i64,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct OnboardingSummaryResponse {
    pub facilities: Vec<FacilityOnboardingSummary>,
}

pub async fn get_onboarding_summary(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
) -> Response {
    let mut tx = try_response!(
        begin_for(
            &state,
            &user,
            "Could not load this company's Onboarding Summary"
        )
        .await
    );

    let rows: Result<Vec<FacilityOnboardingSummary>, sqlx::Error> = sqlx::query_as(
        "SELECT f.id AS facility_id, f.name AS facility_name,
                (fma.facility_id IS NOT NULL) AS elavon_linked,
                next_step.task_name AS elavon_next_step,
                (next_step.id IS NOT NULL AND next_step.id = qms_task.id) AS elavon_awaiting_credentials,
                (qms_task.id IS NOT NULL AND NOT EXISTS (
                   SELECT 1 FROM clients.ps_task_status
                    WHERE facility_id = f.id AND workflow = 'merchant_account'
                      AND NOT hidden AND status <> 'Completed'
                      AND lower(btrim(task_name)) IN (
                            SELECT lower(btrim(task_name))
                              FROM integrations.ps_task_role_name
                             WHERE role = 'qms_credentials')
                )) AS elavon_complete,
                COALESCE(dedup_counts.run_count, 0) AS duplicate_checks_completed
           FROM clients.facilities f
           LEFT JOIN clients.facility_merchant_accounts fma ON fma.facility_id = f.id
           LEFT JOIN LATERAL (
             SELECT id
               FROM clients.ps_task_status
              WHERE facility_id = f.id AND workflow = 'merchant_account' AND NOT hidden
                AND lower(btrim(task_name)) IN (
                      SELECT lower(btrim(task_name))
                        FROM integrations.ps_task_role_name
                       WHERE role = 'qms_credentials')
              ORDER BY id ASC
              LIMIT 1
           ) qms_task ON true
           LEFT JOIN LATERAL (
             SELECT id, task_name
               FROM clients.ps_task_status
              WHERE facility_id = f.id AND workflow = 'merchant_account' AND status <> 'Completed'
                AND NOT hidden
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
