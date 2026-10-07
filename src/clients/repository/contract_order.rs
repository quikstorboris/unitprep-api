//! Contract Order run ingestion (the workflow is on hold).

use crate::clients::contract_order_mapping::MappedContractOrder;
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Inserts a Contract Order run's data for an already-existing
/// facility. Nothing here is encrypted -- this workflow has no
/// sensitive fields, unlike New Merchant Account.
///
/// : Contract Order ingestion is deliberately ON HOLD
/// (no endpoint calls this yet); it is kept, with its live-DB test, rather
/// than deleted. Remove the allow when a caller exists.
#[allow(dead_code)]
pub async fn ingest_contract_order_run(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    mapped: &MappedContractOrder,
    ps_contract_order_run_id: &str,
    raw_ps_snapshot: &Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO clients.facility_contract_orders
            (facility_id, migrating_from_system, source, ps_contract_order_run_id,
             raw_ps_snapshot, last_synced_at)
         VALUES ($1, $2, 'process_street', $3, $4, now())",
    )
    .bind(facility_id)
    .bind(&mapped.migrating_from_system)
    .bind(ps_contract_order_run_id)
    .bind(raw_ps_snapshot)
    .execute(&mut **tx)
    .await?;

    Ok(())
}
