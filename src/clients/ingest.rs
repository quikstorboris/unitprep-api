//! Orchestrates a full facility import: calls the live Process Street
//! API via `ProcessStreetClient`, maps the response, and writes it into
//! the `clients` schema -- the actual "trigger" Phase 1 exists to
//! prove. Not yet wired to anything (no HTTP handler, no CLI binary),
//! but confirmed working against the real, live API (see
//! `live_tests::ingest_facility_works_against_the_live_api`, `#[ignore]`d
//! and run explicitly with a real `PROCESS_STREET_API_KEY`) -- real
//! network calls, real encryption, a real Postgres write, then rolled
//! back so nothing persists. `clients::repository`'s own separate
//! `#[ignore]`d integration test proves the mapping -> DB half of this
//! same path in isolation, using captured fixture data instead.
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
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

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
/// Takes an already-open transaction rather than a pool, matching every
/// function in `clients::repository` -- the caller decides when to
/// begin the RLS transaction and whether to commit or roll it back, the
/// same discipline used everywhere else in this module. This also
/// means a partial import (writes for one workflow succeeded, a later
/// API call failed) is never left half-committed: an error from this
/// function leaves the transaction uncommitted, and the caller's own
/// rollback (explicit, or a dropped transaction) discards everything
/// written so far, rather than looking like a completed, verified
/// client record.
pub async fn ingest_facility(
    client: &ProcessStreetClient,
    tx: &mut Transaction<'_, Postgres>,
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

    // The full raw PS field response, unfiltered, for the Intake run --
    // safe to keep verbatim since nothing sensitive lives in this
    // workflow (unlike Merchant Account, which builds its own sanitized
    // snapshot inside merchant_account_mapping).
    let intake_snapshot: Value = serde_json::to_value(&intake_fields).unwrap_or(Value::Null);

    let (company_id, facility_id) =
        ingest_intake_run(tx, &mapped_intake, intake_run_id, &intake_snapshot).await?;
    upsert_task_status(tx, facility_id, "intake", &intake_tasks).await?;

    let had_merchant_account = if let Some((mapped_nma, nma_tasks)) = merchant_account {
        ingest_merchant_account_run(tx, facility_id, &mapped_nma, merchant_account_run_id.unwrap())
            .await?;
        upsert_task_status(tx, facility_id, "merchant_account", &nma_tasks).await?;
        true
    } else {
        false
    };

    let had_contract_order = if let Some((mapped_co, co_tasks, co_snapshot)) = contract_order {
        ingest_contract_order_run(
            tx,
            facility_id,
            &mapped_co,
            contract_order_run_id.unwrap(),
            &co_snapshot,
        )
        .await?;
        upsert_task_status(tx, facility_id, "contract_order", &co_tasks).await?;
        true
    } else {
        false
    };

    Ok(IngestedFacility {
        company_id,
        facility_id,
        had_merchant_account,
        had_contract_order,
    })
}

#[cfg(test)]
mod live_tests {
    use serde_json::Value;
    use serial_test::serial;
    use uuid::Uuid;

    use crate::process_street::{ProcessStreetClient, ProcessStreetConfig};

    use super::*;

    fn set_test_key() {
        std::env::set_var(
            "CLIENT_PII_ENCRYPTION_KEY",
            "3333333333333333333333333333333333333333333333333333333333333333",
        );
    }
    fn clear_test_key() {
        std::env::remove_var("CLIENT_PII_ENCRYPTION_KEY");
    }

    /// The one test in this whole integration that actually calls the
    /// live Process Street API -- every other test uses captured/
    /// sanitized fixtures. Proves `ingest_facility` works end to end
    /// against the real network + the real, migrated Postgres schema
    /// for a known real facility (Prairie Enterprises / Highway 20),
    /// then rolls back so nothing persists -- this is a one-time
    /// verification, not a claim that Highway 20 should actually exist
    /// in `clients` yet (that's a deliberate "Add to OO" action, Phase 3,
    /// not something a check like this should do as a side effect).
    ///
    /// Deliberately does NOT assert against any of Highway 20's real
    /// sensitive values (EIN/SSN/bank numbers) -- unlike the other
    /// tests in this crate, which use sanitized fixtures precisely so a
    /// real value never has to be written into committed source. This
    /// test only confirms shapes/non-real-secret facts (company name,
    /// facility name, decryption succeeding) that are safe to commit.
    ///
    /// Needs both a real, reachable Postgres with every migration
    /// applied AND a real `PROCESS_STREET_API_KEY` -- `#[ignore]`d so
    /// the fast offline suite stays fast and offline. Run explicitly
    /// with `cargo test -- --ignored ingest_facility_works_against_the_live_api`.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres AND a real Process Street API key -- see doc comment"]
    #[serial(client_pii_encryption_key_env)]
    async fn ingest_facility_works_against_the_live_api() {
        let _ = dotenvy::from_filename(".env.local");
        set_test_key();

        let ps_config =
            ProcessStreetConfig::from_env().expect("PROCESS_STREET_API_KEY must be set in .env.local");
        let client = ProcessStreetClient::new(ps_config);

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let user_id = Uuid::new_v4();
        let mut tx = crate::auth::begin_rls_transaction(
            &db,
            user_id,
            &["onboarding_manager".to_string()],
        )
        .await
        .expect("beginning an RLS transaction must succeed");

        let result = ingest_facility(
            &client,
            &mut tx,
            "iy22NyiqGjwAAytKp0NErQ",       // Highway 20 Intake/Progress
            Some("n1JtiN4m3mP-I0j8BChG4A"), // Highway 20 New Merchant Account
            // Highway 20 does have a real Contract Order run
            // (discovered 2026-08-31, see clients::search's own test),
            // deliberately left out here -- Contract Order work is on
            // hold per Boris's explicit call, not omitted because no
            // run exists.
            None,
        )
        .await
        .expect("ingest_facility must succeed against the live API");

        assert!(result.had_merchant_account);
        assert!(!result.had_contract_order);

        // Spot-check the live data matches what this session already
        // knows about Highway 20 from earlier direct API exploration --
        // proves the live call returned real, sensible data, not just
        // "some response that happened to parse." Company/facility name
        // are business-identity facts, not secrets -- safe to assert on
        // directly, unlike the encrypted fields below.
        let (legal_name,): (String,) =
            sqlx::query_as("SELECT legal_name FROM clients.companies WHERE id = $1")
                .bind(result.company_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(legal_name, "Prairie Enterprises LLC");

        let (facility_name,): (String,) =
            sqlx::query_as("SELECT name FROM clients.facilities WHERE id = $1")
                .bind(result.facility_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(facility_name, "Highway 20 Self Storage");

        // The live Merchant Account data is genuinely sensitive (real
        // SSNs/EIN/bank numbers) -- confirm it actually encrypted and
        // round-trips, without ever asserting against (or printing) the
        // real decrypted value itself.
        let (encrypted_secrets,): (Option<Vec<u8>>,) = sqlx::query_as(
            "SELECT encrypted_secrets FROM clients.facility_merchant_accounts WHERE facility_id = $1",
        )
        .bind(result.facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let decrypted = crate::clients::encryption::decrypt(
            result.facility_id.as_bytes(),
            &encrypted_secrets.expect("Highway 20's live NMA run has real secrets"),
        )
        .expect("live-ingested facility secrets must decrypt");
        let secrets_json: Value = serde_json::from_slice(&decrypted).unwrap();
        assert!(
            secrets_json["ein"].as_str().is_some_and(|s| !s.is_empty()),
            "a real EIN must have round-tripped through encryption, without asserting its actual value"
        );

        tx.rollback()
            .await
            .expect("rollback must succeed -- this is a one-time check, not a real import");
        clear_test_key();
    }
}
