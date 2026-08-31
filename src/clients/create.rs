//! The real "Add to OO" trigger for Phase 3's confirmation screen --
//! given exactly one Intake run designated the company source and zero
//! or more other Intake runs designated facilities, creates one real
//! `clients.companies` row and one `clients.facilities` row per
//! facility run, all attached to that same company.
//!
//! **Company vs. Facility is a real either/or, not "one row does
//! both"** (Boris, 2026-08-31): the company-designated run contributes
//! only its corporate fields (`MappedCompany`) -- its own facility
//! fields, fees, taxes, delinquency steps, etc. are deliberately never
//! written. If that same real location should also become a facility,
//! it gets selected again in a later pass, marked Facility that time.
//! This is why `resolve_company_name` is called with `merchant_account:
//! None` below -- Merchant-Account candidate matching (finding the
//! right New Merchant Account run for a facility, confirming it with
//! the user before it can be linked) is designed but not built yet, so
//! the sole-proprietor DBA-naming rule can't actually fire from this
//! flow yet either; it will once that matching flow exists and this
//! call site is updated to pass real Merchant Account data through.
//!
//! **Adding a facility to a company that already exists in OO** (not
//! part of this batch) isn't handled here -- per Boris's own call,
//! that's a separate search pulling the facility in on its own, not a
//! feature of this endpoint. `company_intake_run_id` is always required;
//! this always creates a new company.

use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::clients::company_naming::resolve_company_name;
use crate::clients::intake_mapping::map_intake_fields;
use crate::clients::repository::{insert_company, insert_facility, insert_facility_policies_and_people};
use crate::process_street::{ProcessStreetClient, ProcessStreetError};

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("Process Street request failed: {0}")]
    ProcessStreet(#[from] ProcessStreetError),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    /// One or more of the selected Intake run ids already has a
    /// `clients.companies`/`clients.facilities` row -- carries every
    /// offending run id so the caller can report exactly which
    /// selection(s) need to be unchecked, not just "something's already
    /// imported."
    #[error("already imported: {0:?}")]
    AlreadyImported(Vec<String>),
}

#[derive(Debug)]
pub struct CreatedFromSelection {
    pub company_id: Uuid,
    pub facility_ids: Vec<Uuid>,
}

/// Checks every involved run id against both `clients.facilities` and
/// `clients.companies` before anything is fetched from PS or written --
/// a duplicate must block the whole batch, not partially import it.
async fn check_not_already_imported(
    tx: &mut Transaction<'_, Postgres>,
    run_ids: &[&str],
) -> Result<(), CreateError> {
    let already: Vec<(String,)> = sqlx::query_as(
        "SELECT ps_intake_run_id FROM clients.facilities WHERE ps_intake_run_id = ANY($1)
         UNION
         SELECT ps_intake_run_id FROM clients.companies WHERE ps_intake_run_id = ANY($1)",
    )
    .bind(run_ids)
    .fetch_all(&mut **tx)
    .await?;

    if already.is_empty() {
        Ok(())
    } else {
        Err(CreateError::AlreadyImported(already.into_iter().map(|(id,)| id).collect()))
    }
}

/// Creates a company from `company_intake_run_id` and a facility for
/// each of `facility_intake_run_ids`, all attached to that company.
/// Takes an already-open transaction -- the caller decides whether to
/// commit or roll back, same discipline every other write in this
/// domain uses (see `clients::ingest::ingest_facility`).
pub async fn create_company_and_facilities(
    client: &ProcessStreetClient,
    tx: &mut Transaction<'_, Postgres>,
    company_intake_run_id: &str,
    facility_intake_run_ids: &[String],
) -> Result<CreatedFromSelection, CreateError> {
    let mut all_run_ids: Vec<&str> = vec![company_intake_run_id];
    all_run_ids.extend(facility_intake_run_ids.iter().map(String::as_str));
    check_not_already_imported(tx, &all_run_ids).await?;

    let company_fields = client.get_run_form_fields(company_intake_run_id).await?;
    let mapped_company_run = map_intake_fields(&company_fields);
    let company_snapshot: Value = serde_json::to_value(&company_fields).unwrap_or(Value::Null);

    // merchant_account: None -- see this module's own doc comment.
    let legal_name = resolve_company_name(mapped_company_run.company.legal_name.as_deref(), None)
        .unwrap_or_else(|| "(unnamed company)".to_string());

    let company_id = insert_company(
        tx,
        &legal_name,
        &mapped_company_run.company,
        company_intake_run_id,
        &company_snapshot,
    )
    .await?;

    let mut facility_ids = Vec::with_capacity(facility_intake_run_ids.len());
    for run_id in facility_intake_run_ids {
        let fields = client.get_run_form_fields(run_id).await?;
        let mapped = map_intake_fields(&fields);
        let snapshot: Value = serde_json::to_value(&fields).unwrap_or(Value::Null);

        let facility_id = insert_facility(tx, company_id, &mapped.facility, run_id, &snapshot).await?;
        insert_facility_policies_and_people(tx, facility_id, &mapped, &snapshot).await?;
        facility_ids.push(facility_id);
    }

    Ok(CreatedFromSelection { company_id, facility_ids })
}

#[cfg(test)]
mod live_tests {
    use serial_test::serial;

    use crate::process_street::{ProcessStreetClient, ProcessStreetConfig};

    use super::*;

    fn set_test_key() {
        std::env::set_var(
            "CLIENT_PII_ENCRYPTION_KEY",
            "4444444444444444444444444444444444444444444444444444444444444444",
        );
    }
    fn clear_test_key() {
        std::env::remove_var("CLIENT_PII_ENCRYPTION_KEY");
    }

    /// Proves the split-creation flow against the real live PS API and
    /// real Postgres: Highway 20 marked as the Company source, no
    /// separate Facility rows selected in this run (so it creates a
    /// company with zero facilities -- an intentionally minimal, valid
    /// case per this module's own "Company is not also a Facility"
    /// design). Rolled back so nothing persists.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres AND a real Process Street API key -- see doc comment"]
    #[serial(client_pii_encryption_key_env)]
    async fn creates_a_company_with_no_facilities_from_a_real_intake_run() {
        let _ = dotenvy::from_filename(".env.local");
        set_test_key();

        let ps_config =
            ProcessStreetConfig::from_env().expect("PROCESS_STREET_API_KEY must be set in .env.local");
        let client = ProcessStreetClient::new(ps_config);

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let user_id = Uuid::new_v4();
        let mut tx = crate::auth::begin_rls_transaction(&db, user_id, &["onboarding_manager".to_string()])
            .await
            .expect("beginning an RLS transaction must succeed");

        let result = create_company_and_facilities(
            &client,
            &mut tx,
            "iy22NyiqGjwAAytKp0NErQ", // Highway 20 Intake/Progress
            &[],
        )
        .await
        .expect("creating a company from the real live run must succeed");

        assert!(result.facility_ids.is_empty());

        let (legal_name,): (String,) =
            sqlx::query_as("SELECT legal_name FROM clients.companies WHERE id = $1")
                .bind(result.company_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(legal_name, "Prairie Enterprises LLC");

        // The company's own run must NOT also have created a facility --
        // exactly zero facilities point at this company.
        let (facility_count,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM clients.facilities WHERE company_id = $1")
                .bind(result.company_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(facility_count, 0);

        tx.rollback()
            .await
            .expect("rollback must succeed -- this is a one-time check, not a real import");
        clear_test_key();
    }

    /// Duplicate-import protection: attempting to create a company from
    /// a run id that (within this same uncommitted transaction) has
    /// already been used must fail with `AlreadyImported`, not silently
    /// create a second company for the same real business.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres AND a real Process Street API key -- see doc comment"]
    #[serial(client_pii_encryption_key_env)]
    async fn refuses_to_recreate_an_already_imported_run() {
        let _ = dotenvy::from_filename(".env.local");
        set_test_key();

        let ps_config =
            ProcessStreetConfig::from_env().expect("PROCESS_STREET_API_KEY must be set in .env.local");
        let client = ProcessStreetClient::new(ps_config);

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let user_id = Uuid::new_v4();
        let mut tx = crate::auth::begin_rls_transaction(&db, user_id, &["onboarding_manager".to_string()])
            .await
            .expect("beginning an RLS transaction must succeed");

        create_company_and_facilities(&client, &mut tx, "iy22NyiqGjwAAytKp0NErQ", &[])
            .await
            .expect("first create must succeed");

        let second_attempt =
            create_company_and_facilities(&client, &mut tx, "iy22NyiqGjwAAytKp0NErQ", &[]).await;

        match second_attempt {
            Err(CreateError::AlreadyImported(ids)) => {
                assert_eq!(ids, vec!["iy22NyiqGjwAAytKp0NErQ".to_string()]);
            }
            other => panic!("expected AlreadyImported, got {other:?}"),
        }

        tx.rollback()
            .await
            .expect("rollback must succeed -- this is a one-time check, not a real import");
        clear_test_key();
    }
}
