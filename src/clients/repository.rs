//! Writes already-mapped Process Street data into the `clients` schema.
//! Every write runs against a caller-supplied RLS transaction (see
//! `auth::authenticated_user::begin_rls_transaction`) -- the same
//! mechanism every other write in this app uses, so these rows are
//! subject to the real onboarding_manager/department_manager RLS
//! gating, never a privileged bypass. Callers commit or roll back the
//! transaction themselves.
//!
//! Plain dynamic `sqlx::query`/`query_as`, matching this codebase's
//! established convention -- there is no compile-time-checked
//! `query!`/`query_as!` usage anywhere in this crate (no `.sqlx` offline
//! cache exists), so this file doesn't introduce one either.
//!
//! Takes only already-mapped, already-encrypted data. This module never
//! imports `FacilitySecrets`/`PartyPii` (private to
//! `merchant_account_mapping`) and never sees a plaintext SSN -- it
//! only ever binds the `Vec<u8>` ciphertext `encrypted_pii`/
//! `encrypted_secrets` already produced by that module.

// Phase 1 only -- no HTTP handler calls into `clients::*` yet. Remove
// once a real caller exists.
#![allow(dead_code)]

use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::clients::contract_order_mapping::MappedContractOrder;
use crate::clients::intake_mapping::MappedIntakeRun;
use crate::clients::merchant_account_mapping::{MappedMerchantAccount, MappedParty};
use crate::clients::people::ParsedPerson;
use crate::process_street::Task;

/// Inserts a company, facility, and every Facility Policies row a
/// `MappedIntakeRun` carries, plus the owner/district-manager/manager
/// people it parsed. Returns `(company_id, facility_id)`.
pub async fn ingest_intake_run(
    tx: &mut Transaction<'_, Postgres>,
    mapped: &MappedIntakeRun,
    ps_intake_run_id: &str,
    raw_ps_snapshot: &Value,
) -> Result<(Uuid, Uuid), sqlx::Error> {
    let (company_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.companies
            (legal_name, corporate_email, corporate_phone, corporate_address_street,
             corporate_address_city, corporate_address_state, corporate_address_zip,
             subdomain, source, ps_intake_run_id, raw_ps_snapshot, last_synced_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'process_street', $9, $10, now())
         RETURNING id",
    )
    .bind(mapped.company.legal_name.as_deref().unwrap_or("(unnamed company)"))
    .bind(&mapped.company.corporate_email)
    .bind(&mapped.company.corporate_phone)
    .bind(&mapped.company.corporate_address_street)
    .bind(&mapped.company.corporate_address_city)
    .bind(&mapped.company.corporate_address_state)
    .bind(&mapped.company.corporate_address_zip)
    .bind(&mapped.company.subdomain)
    .bind(ps_intake_run_id)
    .bind(raw_ps_snapshot)
    .fetch_one(&mut **tx)
    .await?;

    let (facility_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.facilities
            (company_id, name, street_address, city, state, zip, phone, email,
             units_count, primary_storage_offering, previous_pms, access_control_system,
             go_live_date, dropbox_folder_url, subdomain, subdomain_exists_in_qms_raw,
             system_email, source, ps_intake_run_id, raw_ps_snapshot, last_synced_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                 $17, 'process_street', $18, $19, now())
         RETURNING id",
    )
    .bind(company_id)
    .bind(mapped.facility.name.as_deref().unwrap_or("(unnamed facility)"))
    .bind(&mapped.facility.street_address)
    .bind(&mapped.facility.city)
    .bind(&mapped.facility.state)
    .bind(&mapped.facility.zip)
    .bind(&mapped.facility.phone)
    .bind(&mapped.facility.email)
    .bind(mapped.facility.units_count)
    .bind(&mapped.facility.primary_storage_offering)
    .bind(&mapped.facility.previous_pms)
    .bind(&mapped.facility.access_control_system)
    .bind(mapped.facility.go_live_date)
    .bind(&mapped.facility.dropbox_folder_url)
    .bind(&mapped.facility.subdomain)
    .bind(&mapped.facility.subdomain_exists_in_qms_raw)
    .bind(&mapped.facility.system_email)
    .bind(ps_intake_run_id)
    .bind(raw_ps_snapshot)
    .fetch_one(&mut **tx)
    .await?;

    sqlx::query("INSERT INTO clients.facility_policies (facility_id, raw_ps_snapshot) VALUES ($1, $2)")
        .bind(facility_id)
        .bind(raw_ps_snapshot)
        .execute(&mut **tx)
        .await?;

    for fee in &mapped.fees {
        sqlx::query(
            "INSERT INTO clients.policy_fees (facility_policies_id, fee_type, label, raw_value)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(facility_id)
        .bind(fee.fee_type)
        .bind(&fee.label)
        .bind(&fee.raw_value)
        .execute(&mut **tx)
        .await?;
    }

    if let Some(taxes) = &mapped.taxes {
        sqlx::query(
            "INSERT INTO clients.policy_taxes
                (facility_policies_id, sales_tax_applies_raw, sales_tax_rate_raw,
                 rent_tax_applies_raw, rent_tax_rate_raw, rent_tax_applies_to_all_units_raw,
                 other_one_time_taxes_raw, other_recurring_taxes_raw)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(facility_id)
        .bind(&taxes.sales_tax_applies_raw)
        .bind(&taxes.sales_tax_rate_raw)
        .bind(&taxes.rent_tax_applies_raw)
        .bind(&taxes.rent_tax_rate_raw)
        .bind(&taxes.rent_tax_applies_to_all_units_raw)
        .bind(&taxes.other_one_time_taxes_raw)
        .bind(&taxes.other_recurring_taxes_raw)
        .execute(&mut **tx)
        .await?;
    }

    for step in &mapped.delinquency_steps {
        sqlx::query(
            "INSERT INTO clients.policy_delinquency_steps
                (facility_policies_id, step_order, step_type, raw_value)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(facility_id)
        .bind(step.step_order)
        .bind(step.step_type)
        .bind(&step.raw_value)
        .execute(&mut **tx)
        .await?;
    }

    for tier in &mapped.coverage_tiers {
        sqlx::query(
            "INSERT INTO clients.policy_coverage_tiers
                (facility_policies_id, tier_number, total_coverage_amount_raw, cost_to_tenant_raw)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(facility_id)
        .bind(tier.tier_number)
        .bind(&tier.total_coverage_amount_raw)
        .bind(&tier.cost_to_tenant_raw)
        .execute(&mut **tx)
        .await?;
    }

    if let Some(commission) = &mapped.commission {
        sqlx::query(
            "INSERT INTO clients.policy_commission
                (facility_policies_id, commission_type_raw, dollar_amount_raw, percent_amount_raw)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(facility_id)
        .bind(&commission.commission_type_raw)
        .bind(&commission.dollar_amount_raw)
        .bind(&commission.percent_amount_raw)
        .execute(&mut **tx)
        .await?;
    }

    if let Some(specials) = &mapped.specials_raw_text {
        sqlx::query("INSERT INTO clients.policy_specials (facility_policies_id, raw_text) VALUES ($1, $2)")
            .bind(facility_id)
            .bind(specials)
            .execute(&mut **tx)
            .await?;
    }

    for (person, role) in mapped
        .owners
        .iter()
        .map(|p| (p, "owner"))
        .chain(mapped.district_managers.iter().map(|p| (p, "district_manager")))
        .chain(mapped.managers.iter().map(|p| (p, "manager")))
    {
        link_person_to_facility(tx, facility_id, person, role).await?;
    }

    Ok((company_id, facility_id))
}

/// Finds an existing person by email (case-insensitive, via CITEXT) or
/// creates one, then links them to the facility with the given role --
/// idempotent via `ON CONFLICT DO NOTHING` on the (facility_id,
/// person_id, role) primary key, since the same person/role pair can
/// legitimately be re-ingested. Matching by name+phone alone (no email)
/// is deliberately NOT attempted here -- that's the same
/// manual-review-worthy fuzzy-match problem [[Dedup Tool Index|the
/// dedup tool]] exists to solve, not something to guess at inline.
async fn link_person_to_facility(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    person: &ParsedPerson,
    role: &str,
) -> Result<(), sqlx::Error> {
    let existing: Option<(Uuid,)> = match &person.email {
        Some(email) => sqlx::query_as("SELECT id FROM clients.people WHERE email = $1")
            .bind(email)
            .fetch_optional(&mut **tx)
            .await?,
        None => None,
    };

    let person_id = match existing {
        Some((id,)) => id,
        None => {
            let (id,): (Uuid,) = sqlx::query_as(
                "INSERT INTO clients.people (full_name, email, phone) VALUES ($1, $2, $3) RETURNING id",
            )
            .bind(&person.full_name)
            .bind(&person.email)
            .bind(&person.phone)
            .fetch_one(&mut **tx)
            .await?;
            id
        }
    };

    sqlx::query(
        "INSERT INTO clients.facility_people (facility_id, person_id, role)
         VALUES ($1, $2, $3)
         ON CONFLICT (facility_id, person_id, role) DO NOTHING",
    )
    .bind(facility_id)
    .bind(person_id)
    .bind(role)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Inserts the Elavon tab's facility-level data (rate/status/encrypted
/// secrets) and every party row (signer, owners, intermediary
/// businesses) for an already-existing facility.
pub async fn ingest_merchant_account_run(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    mapped: &MappedMerchantAccount,
    ps_new_merchant_run_id: &str,
) -> Result<(), sqlx::Error> {
    let encrypted_secrets = mapped
        .encrypted_secrets(facility_id)
        .expect("CLIENT_PII_ENCRYPTION_KEY must be configured before ingesting a Merchant Account run");

    sqlx::query(
        "INSERT INTO clients.facility_merchant_accounts
            (facility_id, rate_provided, application_status, source, ps_new_merchant_run_id,
             raw_ps_snapshot, encrypted_secrets, last_synced_at)
         VALUES ($1, $2, $3, 'process_street', $4, $5, $6, now())",
    )
    .bind(facility_id)
    .bind(&mapped.rate_provided)
    .bind(&mapped.application_status)
    .bind(ps_new_merchant_run_id)
    .bind(&mapped.sanitized_snapshot)
    .bind(encrypted_secrets)
    .execute(&mut **tx)
    .await?;

    for party in &mapped.parties {
        insert_party(tx, facility_id, party, ps_new_merchant_run_id).await?;
    }

    Ok(())
}

async fn insert_party(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    party: &MappedParty,
    ps_new_merchant_run_id: &str,
) -> Result<(), sqlx::Error> {
    let encrypted_pii = party
        .encrypted_pii(facility_id)
        .expect("CLIENT_PII_ENCRYPTION_KEY must be configured before ingesting a Merchant Account run");

    sqlx::query(
        "INSERT INTO clients.facility_merchant_account_parties
            (facility_id, party_role, party_index, display_name, title, ownership_percent,
             email, phone, country_of_citizenship, country, encrypted_pii, source,
             ps_new_merchant_run_id, last_synced_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'process_street', $12, now())",
    )
    .bind(facility_id)
    .bind(party.party_role)
    .bind(party.party_index)
    .bind(&party.display_name)
    .bind(&party.title)
    .bind(party.ownership_percent)
    .bind(&party.email)
    .bind(&party.phone)
    .bind(&party.country_of_citizenship)
    .bind(&party.country)
    .bind(encrypted_pii)
    .bind(ps_new_merchant_run_id)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Inserts a Contract Order run's data for an already-existing
/// facility. Nothing here is encrypted -- this workflow has no
/// sensitive fields, unlike New Merchant Account.
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
                (facility_id, workflow, ps_task_id, task_name, status, last_synced_at)
             VALUES ($1, $2, $3, $4, $5, now())
             ON CONFLICT (facility_id, workflow, ps_task_id)
             DO UPDATE SET task_name = EXCLUDED.task_name,
                           status = EXCLUDED.status,
                           last_synced_at = now()",
        )
        .bind(facility_id)
        .bind(workflow)
        .bind(&task.id)
        .bind(&task.name)
        .bind(&task.status)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod integration_tests {
    use serde_json::Value;
    use serial_test::serial;
    use uuid::Uuid;

    use crate::clients::contract_order_mapping::map_contract_order_fields;
    use crate::clients::intake_mapping::map_intake_fields;
    use crate::clients::merchant_account_mapping::map_merchant_account_fields;
    use crate::process_street::{FormField, Task};

    use super::*;

    // Same fixtures the unit tests in intake_mapping/merchant_account_mapping
    // use -- see those modules' own doc comments on why each is safe to
    // commit (Intake has no sensitive data at all; the Merchant Account
    // fixture has every sensitive value replaced with an obvious fake
    // before it was ever written to disk).
    const HIGHWAY20_INTAKE_FIELDS: &str =
        include_str!("testdata/highway20_intake_fields.json");
    const HIGHWAY20_INTAKE_TASKS: &str = include_str!("testdata/highway20_intake_tasks.json");
    const HIGHWAY20_NMA_FIELDS_SANITIZED: &str =
        include_str!("testdata/highway20_merchant_account_fields_sanitized.json");
    // A real Contract Order run for a different real client (Tri County
    // Mini Storage). Highway 20 does actually have its own real Contract
    // Order run too (discovered 2026-08-31 while testing clients::search
    // -- an earlier belief that it didn't was itself a casualty of the
    // status=Active-only bug this same session found and fixed), but
    // that data stays untouched here per Boris's explicit hold on
    // further Contract Order work. Tri County's run is grafted onto
    // Highway 20's facility_id purely to prove
    // ingest_contract_order_run's SQL is valid against the real,
    // migrated schema, the same reasoning
    // auth::authenticated_user's own `query_sessions_own_sql_is_valid_
    // against_the_real_schema` test uses.
    const TRI_COUNTY_CONTRACT_ORDER_FIELDS: &str =
        include_str!("testdata/tri_county_contract_order_fields.json");

    fn set_test_key() {
        std::env::set_var(
            "CLIENT_PII_ENCRYPTION_KEY",
            "2222222222222222222222222222222222222222222222222222222222222222",
        );
    }
    fn clear_test_key() {
        std::env::remove_var("CLIENT_PII_ENCRYPTION_KEY");
    }

    /// Full Phase 1 pipeline, proven end to end against the real,
    /// migrated `clients` schema on the real Neon dev branch -- not a
    /// mock, not an in-memory pool. Ingests the real (Intake) /
    /// sanitized-but-realistic (Merchant Account) Prairie Enterprises
    /// Highway 20 fixtures through the actual mapping + repository +
    /// RLS-transaction path a real request would use, then verifies
    /// the rows really landed correctly -- including decrypting the
    /// encrypted PII/secrets blobs back out of Postgres and confirming
    /// they match what was ingested -- before rolling back so nothing
    /// persists.
    ///
    /// Needs a real, reachable Postgres with every migration applied
    /// (`DATABASE_URL` from `.env.local`) -- `#[ignore]`d so the fast
    /// offline suite this crate otherwise is stays fast and offline.
    /// Run explicitly with `cargo test -- --ignored highway20_golden_fixture`.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
    #[serial(client_pii_encryption_key_env)]
    async fn highway20_golden_fixture_ingests_and_round_trips_through_real_postgres() {
        let _ = dotenvy::from_filename(".env.local");
        set_test_key();

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let user_id = Uuid::new_v4();
        let mut tx = crate::auth::begin_rls_transaction(
            &db,
            user_id,
            &["onboarding_manager".to_string()],
        )
        .await
        .expect("beginning an RLS transaction must succeed");

        let intake_fields: Vec<FormField> =
            serde_json::from_str(HIGHWAY20_INTAKE_FIELDS).expect("intake fixture must parse");
        let intake_tasks: Vec<Task> =
            serde_json::from_value(serde_json::from_str::<Value>(HIGHWAY20_INTAKE_TASKS).unwrap()["tasks"].take())
                .expect("intake tasks fixture must parse");
        let nma_fields: Vec<FormField> =
            serde_json::from_str(HIGHWAY20_NMA_FIELDS_SANITIZED).expect("NMA fixture must parse");

        let mapped_intake = map_intake_fields(&intake_fields);
        let mapped_nma = map_merchant_account_fields(&nma_fields);

        let (company_id, facility_id) = ingest_intake_run(
            &mut tx,
            &mapped_intake,
            "iy22NyiqGjwAAytKp0NErQ",
            &Value::Null,
        )
        .await
        .expect("ingesting the real Intake run must succeed");

        ingest_merchant_account_run(&mut tx, facility_id, &mapped_nma, "n1JtiN4m3mP-I0j8BChG4A")
            .await
            .expect("ingesting the sanitized Merchant Account run must succeed");

        upsert_task_status(&mut tx, facility_id, "intake", &intake_tasks)
            .await
            .expect("upserting task status must succeed");

        // --- Verify plain data landed correctly ---
        let (legal_name,): (String,) =
            sqlx::query_as("SELECT legal_name FROM clients.companies WHERE id = $1")
                .bind(company_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(legal_name, "Prairie Enterprises LLC");

        let (facility_name, units_count): (String, Option<i32>) = sqlx::query_as(
            "SELECT name, units_count FROM clients.facilities WHERE id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(facility_name, "Highway 20 Self Storage");
        assert_eq!(units_count, Some(788));

        let (company_subdomain,): (Option<String>,) =
            sqlx::query_as("SELECT subdomain FROM clients.companies WHERE id = $1")
                .bind(company_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(company_subdomain.as_deref(), Some("prairie-enterprises.qms-email.com"));

        let (facility_subdomain, subdomain_exists_raw, system_email): (
            Option<String>,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT subdomain, subdomain_exists_in_qms_raw, system_email
               FROM clients.facilities WHERE id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(facility_subdomain.as_deref(), Some("tenant.highway20selfstorage.com"));
        assert_eq!(subdomain_exists_raw.as_deref(), Some("No"));
        assert_eq!(system_email.as_deref(), Some("info@tenant.highway20selfstorage.com"));

        let (fee_count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM clients.policy_fees WHERE facility_policies_id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert!(fee_count >= 5, "named fees plus the Any Other Fees blob must all be present");

        let (tier_count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM clients.policy_coverage_tiers WHERE facility_policies_id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(tier_count, 5);

        let (task_count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM clients.ps_task_status WHERE facility_id = $1 AND workflow = 'intake'",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(task_count as usize, intake_tasks.len());

        // --- Verify the encrypted columns really round-trip through real Postgres ---
        let (encrypted_secrets,): (Option<Vec<u8>>,) = sqlx::query_as(
            "SELECT encrypted_secrets FROM clients.facility_merchant_accounts WHERE facility_id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let decrypted_secrets =
            crate::clients::encryption::decrypt(facility_id.as_bytes(), &encrypted_secrets.unwrap())
                .expect("facility secrets stored in real Postgres must decrypt");
        let secrets_json: Value = serde_json::from_slice(&decrypted_secrets).unwrap();
        assert_eq!(secrets_json["ein"], "111111111"); // the fixture's fake EIN

        let (owner1_encrypted_pii,): (Option<Vec<u8>>,) = sqlx::query_as(
            "SELECT encrypted_pii FROM clients.facility_merchant_account_parties
             WHERE facility_id = $1 AND party_role = 'owner' AND party_index = 1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let owner1_aad = format!("{facility_id}:owner:1");
        let decrypted_pii =
            crate::clients::encryption::decrypt(owner1_aad.as_bytes(), &owner1_encrypted_pii.unwrap())
                .expect("owner 1's PII stored in real Postgres must decrypt under its own AAD");
        let pii_json: Value = serde_json::from_slice(&decrypted_pii).unwrap();
        assert_eq!(pii_json["ssn"], "000000000"); // the fixture's fake SSN

        // --- Verify raw_ps_snapshot on the Merchant Account row never carries a sensitive key ---
        let (raw_snapshot,): (Value,) = sqlx::query_as(
            "SELECT raw_ps_snapshot FROM clients.facility_merchant_accounts WHERE facility_id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert!(!raw_snapshot.to_string().contains("111111111")); // the fake EIN must not leak into the plaintext snapshot

        // --- Verify ingest_contract_order_run's SQL is valid against the real schema ---
        let tri_county_fields: Vec<FormField> = serde_json::from_str(TRI_COUNTY_CONTRACT_ORDER_FIELDS)
            .expect("Tri County contract order fixture must parse");
        let mapped_contract_order = map_contract_order_fields(&tri_county_fields);
        let contract_order_snapshot: Value =
            serde_json::to_value(&tri_county_fields).unwrap_or(Value::Null);

        ingest_contract_order_run(
            &mut tx,
            facility_id,
            &mapped_contract_order,
            "iz7Jz_awRApa68WuMjtKHw",
            &contract_order_snapshot,
        )
        .await
        .expect("ingesting a real Contract Order run must succeed");

        let (stored_run_id,): (String,) = sqlx::query_as(
            "SELECT ps_contract_order_run_id FROM clients.facility_contract_orders WHERE facility_id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(stored_run_id, "iz7Jz_awRApa68WuMjtKHw");

        tx.rollback().await.expect("rollback must succeed -- this test writes no real data");
        clear_test_key();
    }
}
