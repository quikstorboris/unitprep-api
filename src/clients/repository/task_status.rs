//! Upserting the checklist task statuses Process Street reports for a run.

use crate::process_street::Task;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Upserts step-completion status for one workflow's tasks against an
/// already-existing facility. `workflow` is `intake` | `merchant_account`
/// | `contract_order`, matching `ps_task_status.workflow`'s CHECK
/// constraint.
pub async fn upsert_task_status(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    workflow: &str,
    tasks: &[Task],
) -> Result<(), sqlx::Error> {
    for task in tasks {
        sqlx::query(
            "INSERT INTO clients.ps_task_status
                (facility_id, workflow, ps_task_id, task_name, status, hidden, last_synced_at)
             VALUES ($1, $2, $3, $4, $5, $6, now())
             ON CONFLICT (facility_id, workflow, ps_task_id)
             DO UPDATE SET task_name = EXCLUDED.task_name,
                           status = EXCLUDED.status,
                           hidden = EXCLUDED.hidden,
                           last_synced_at = now()",
        )
        .bind(facility_id)
        .bind(workflow)
        .bind(&task.id)
        .bind(&task.name)
        .bind(&task.status)
        .bind(task.hidden)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}
