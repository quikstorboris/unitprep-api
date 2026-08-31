//! Orchestrates a full facility import: calls the live Process Street
//! API via `ProcessStreetClient`, maps the response, and writes it into
//! the `clients` schema -- the actual "trigger" Phase 1 exists to
//! prove. Not yet wired to anything (no HTTP handler, no CLI binary) --
//! `PROCESS_STREET_API_KEY` isn't in `.env.local` yet, so this has never
//! been run against the live API. `clients::repository`'s own
//! `#[ignore]`d integration test proves the mapping -> DB half of this
//! path against real Postgres using captured fixture data instead.
//!
//! **Any caller resolving run ids for this function must search across
//! Active + Completed + Archived status, not just the API's default
//! (Active only)** -- Contract Order runs in particular are marked
//! Completed once the order is processed, so an Active-only search
//! finds essentially none of them. Confirmed directly against two real
//! clients (Tri County Mini Storage, Dubuqueland Mini Storage) while
//! building `contract_order_mapping`. This isn't specific to Contract
//! Order -- it's how `GET /workflow-runs` behaves for every workflow.

// Phase 1 only -- no HTTP handler or CLI binary calls into this yet.
// Remove once a real caller exists.
#![allow(dead_code)]

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::begin_rls_transaction;
use crate::clients::contract_order_mapping::map_contract_order_fields;
use crate::clients::intake_mapping::map_intake_fields;
use crate::clients::merchant_account_mapping::map_merchant_account_fields;
use crate::clients::repository::{
    ingest_contract_order_run, ingest_intake_run, ingest_merchant_account_run, upsert_task_status,
};
use crate::process_street::{ProcessStreetClient, ProcessStreetError};

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("Process Street request failed: {0}")]
    ProcessStreet(#[from] ProcessStreetError),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

/// Result of a full facility import -- enough for a caller to redirect
/// to the new client record (Phase 3, not built yet) or log what
/// happened.
pub struct IngestedFacility {
    pub company_id: Uuid,
    pub facility_id: Uuid,
    pub had_merchant_account: bool,
    pub had_contract_order: bool,
}

/// Imports one facility from its known PS run ids. `merchant_account_run_id`
/// and `contract_order_run_id` are both optional -- not every facility
/// has reached either stage, and that's expected, not a gap (see
/// [[Process Street Integration — Kickoff & Findings]]).
///
/// Runs everything in one RLS transaction and commits only if every
/// step succeeds -- a partial import (a company/facility missing one
/// workflow's data because that API call failed partway) is worse than
/// no import at all, since it would look like a completed, verified
/// client record.
pub async fn ingest_facility(
    client: &ProcessStreetClient,
    db: &PgPool,
    actor_user_id: Uuid,
    intake_run_id: &str,
    merchant_account_run_id: Option<&str>,
    contract_order_run_id: Option<&str>,
) -> Result<IngestedFacility, IngestError> {
    let intake_fields = client.get_run_form_fields(intake_run_id).await?;
    let intake_tasks = client.get_run_tasks(intake_run_id).await?;
    let mapped_intake = map_intake_fields(&intake_fields);

    let merchant_account = match merchant_account_run_id {
        Some(run_id) => {
            let nma_fields = client.get_run_form_fields(run_id).await?;
            let nma_tasks = client.get_run_tasks(run_id).await?;
            Some((map_merchant_account_fields(&nma_fields), nma_tasks))
        }
        None => None,
    };

    let contract_order = match contract_order_run_id {
        Some(run_id) => {
            let co_fields = client.get_run_form_fields(run_id).await?;
            let co_tasks = client.get_run_tasks(run_id).await?;
            let co_snapshot: Value = serde_json::to_value(&co_fields).unwrap_or(Value::Null);
            Some((map_contract_order_fields(&co_fields), co_tasks, co_snapshot))
        }
        None => None,
    };

    let mut tx = begin_rls_transaction(db, actor_user_id, &["onboarding_manager".to_string()]).await?;

    // The full raw PS field response, unfiltered, for the Intake run --
    // safe to keep verbatim since nothing sensitive lives in this
    // workflow (unlike Merchant Account, which builds its own sanitized
    // snapshot inside merchant_account_mapping).
    let intake_snapshot: Value = serde_json::to_value(&intake_fields).unwrap_or(Value::Null);

    let (company_id, facility_id) =
        ingest_intake_run(&mut tx, &mapped_intake, intake_run_id, &intake_snapshot).await?;
    upsert_task_status(&mut tx, facility_id, "intake", &intake_tasks).await?;

    let had_merchant_account = if let Some((mapped_nma, nma_tasks)) = merchant_account {
        ingest_merchant_account_run(&mut tx, facility_id, &mapped_nma, merchant_account_run_id.unwrap())
            .await?;
        upsert_task_status(&mut tx, facility_id, "merchant_account", &nma_tasks).await?;
        true
    } else {
        false
    };

    let had_contract_order = if let Some((mapped_co, co_tasks, co_snapshot)) = contract_order {
        ingest_contract_order_run(
            &mut tx,
            facility_id,
            &mapped_co,
            contract_order_run_id.unwrap(),
            &co_snapshot,
        )
        .await?;
        upsert_task_status(&mut tx, facility_id, "contract_order", &co_tasks).await?;
        true
    } else {
        false
    };

    tx.commit().await?;

    Ok(IngestedFacility {
        company_id,
        facility_id,
        had_merchant_account,
        had_contract_order,
    })
}
