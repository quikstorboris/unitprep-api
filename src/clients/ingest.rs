//! Orchestrates a full facility import: calls the live Process Street
//! API via `ProcessStreetClient`, maps the response, and writes it into
//! the `clients` schema -- the actual "trigger" Phase 1 exists to
//! prove. Not yet wired to anything (no HTTP handler, no CLI binary) --
//! `PROCESS_STREET_API_KEY` isn't in `.env.local` yet, so this has never
//! been run against the live API. `clients::repository`'s own
//! `#[ignore]`d integration test proves the mapping -> DB half of this
//! path against real Postgres using captured fixture data instead.

// Phase 1 only -- no HTTP handler or CLI binary calls into this yet.
// Remove once a real caller exists.
#![allow(dead_code)]

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::begin_rls_transaction;
use crate::clients::intake_mapping::map_intake_fields;
use crate::clients::merchant_account_mapping::map_merchant_account_fields;
use crate::clients::repository::{ingest_intake_run, ingest_merchant_account_run, upsert_task_status};
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
}

/// Imports one facility from its known PS run ids. `merchant_account_run_id`
/// is optional -- not every facility has reached that stage (see
/// [[Process Street Integration — Kickoff & Findings]] on Contract Order's
/// same absence being normal, not a gap; New Merchant Account is
/// similarly allowed to not exist yet for a brand-new facility).
///
/// Runs everything in one RLS transaction and commits only if every
/// step succeeds -- a partial import (a company/facility with no
/// Merchant Account data because the second API call failed partway)
/// is worse than no import at all, since it would look like a
/// completed, verified client record.
///
/// Contract Order import is deliberately not included here yet -- its
/// real PS field keys were never captured this session (the one
/// facility this pipeline was validated against, Prairie Enterprises /
/// Highway 20, has no Contract Order run to examine), and guessing at
/// field keys blind is exactly the mistake this whole integration has
/// been built by avoiding. Add `ingest_contract_order_run` here once a
/// real run's fields have actually been pulled and checked.
pub async fn ingest_facility(
    client: &ProcessStreetClient,
    db: &PgPool,
    actor_user_id: Uuid,
    intake_run_id: &str,
    merchant_account_run_id: Option<&str>,
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

    tx.commit().await?;

    Ok(IngestedFacility {
        company_id,
        facility_id,
        had_merchant_account,
    })
}
