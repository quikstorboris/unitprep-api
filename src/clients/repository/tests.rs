use serial_test::serial;

use crate::clients::contract_order_mapping::map_contract_order_fields;
use crate::clients::intake_mapping::map_intake_fields;
use crate::clients::merchant_account_mapping::map_merchant_account_fields;
use crate::process_street::FormField;

use serde_json::Value;
use uuid::Uuid;

use super::contract_order::*;
use super::facility::*;
use super::merchant_account::*;
use super::people::*;
use super::task_status::*;
use crate::clients::people::PersonAssignment;
use crate::process_street::Task;

// Same fixtures the unit tests in intake_mapping/merchant_account_mapping
// use -- see those modules' own doc comments on why each is safe to
// commit (Intake has no sensitive data at all; the Merchant Account
// fixture has every sensitive value replaced with an obvious fake
// before it was ever written to disk).
const HIGHWAY20_INTAKE_FIELDS: &str = include_str!("../testdata/highway20_intake_fields.json");
const HIGHWAY20_INTAKE_TASKS: &str = include_str!("../testdata/highway20_intake_tasks.json");
const HIGHWAY20_NMA_FIELDS_SANITIZED: &str =
    include_str!("../testdata/highway20_merchant_account_fields_sanitized.json");
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
    include_str!("../testdata/tri_county_contract_order_fields.json");

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

    let db = crate::db::connect_test();
    let user_id = Uuid::new_v4();
    let mut tx =
        crate::auth::begin_rls_transaction(&db, user_id, &["onboarding_manager".to_string()])
            .await
            .expect("beginning an RLS transaction must succeed");

    let intake_fields: Vec<FormField> =
        serde_json::from_str(HIGHWAY20_INTAKE_FIELDS).expect("intake fixture must parse");
    let intake_tasks: Vec<Task> = serde_json::from_value(
        serde_json::from_str::<Value>(HIGHWAY20_INTAKE_TASKS).unwrap()["tasks"].take(),
    )
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

    ingest_merchant_account_run(
        &mut tx,
        facility_id,
        &mapped_nma,
        "n1JtiN4m3mP-I0j8BChG4A",
        true,
    )
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

    let (facility_name, units_count): (String, Option<i32>) =
        sqlx::query_as("SELECT name, units_count FROM clients.facilities WHERE id = $1")
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
    assert_eq!(
        company_subdomain.as_deref(),
        Some("prairie-enterprises.qms-email.com")
    );

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
    assert_eq!(
        facility_subdomain.as_deref(),
        Some("tenant.highway20selfstorage.com")
    );
    assert_eq!(subdomain_exists_raw.as_deref(), Some("No"));
    assert_eq!(
        system_email.as_deref(),
        Some("info@tenant.highway20selfstorage.com")
    );

    let (fee_count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM clients.policy_fees WHERE facility_policies_id = $1")
            .bind(facility_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert!(
        fee_count >= 5,
        "named fees plus the Any Other Fees blob must all be present"
    );

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

    // ownership_percent is NUMERIC in Postgres -- sqlx has no
    // built-in decode from NUMERIC into plain f64, so every real
    // reader (api::clients_detail, api::clients_elavon) casts to
    // float8 in SQL. Regression coverage for the 2026-09-03 bug
    // where this decode error only ever surfaced once a facility
    // had a real party row for the first time (Boris's own live
    // Elavon-tab link, not caught by this test until now since it
    // never previously selected this column back out at all).
    let (owner1_ownership_percent,): (Option<f64>,) = sqlx::query_as(
        "SELECT ownership_percent::float8 FROM clients.facility_merchant_account_parties
         WHERE facility_id = $1 AND party_role = 'owner' AND party_index = 1",
    )
    .bind(facility_id)
    .fetch_one(&mut *tx)
    .await
    .expect("selecting ownership_percent::float8 back out of real Postgres must not error");
    assert_eq!(owner1_ownership_percent, Some(30.0));

    // credentials_added_to_qms passed in as `true` above must
    // actually land in the row, not silently default to the
    // schema's own `false` -- regression coverage for the
    // 2026-09-03 bug where this column was never in the INSERT's
    // column list at all.
    let (credentials_added_to_qms,): (bool,) = sqlx::query_as(
        "SELECT credentials_added_to_qms FROM clients.facility_merchant_accounts WHERE facility_id = $1",
    )
    .bind(facility_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert!(credentials_added_to_qms);

    // Revenue/volume fields (2026-09-03) -- regression coverage for
    // the bug where these were already in raw_ps_snapshot (never
    // sensitive, so never denylisted) but never had a named column
    // to land in, so the Elavon tab never showed them.
    let (revenue, ach_volume): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT total_annual_business_revenue_raw, annual_electronic_check_volume_raw
           FROM clients.facility_merchant_accounts WHERE facility_id = $1",
    )
    .bind(facility_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(revenue.as_deref(), Some("840000"));
    assert_eq!(ach_volume.as_deref(), Some("20000"));

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

    tx.rollback()
        .await
        .expect("rollback must succeed -- this test writes no real data");
    clear_test_key();
}

/// Proves `upsert_person_and_link_to_facility`'s actual point against
/// real Postgres, post-2026-09-08: a second call for the same email
/// AND the same name only refreshes phone, never creates a duplicate
/// row. Needs a real, reachable Postgres with every migration
/// applied, same as the golden fixture test above -- `#[ignore]`d for
/// the same reason.
#[tokio::test]
#[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
async fn upsert_person_and_link_to_facility_refreshes_phone_on_a_second_call_with_the_same_name() {
    let _ = dotenvy::from_filename(".env.local");

    let db = crate::db::connect_test();
    let user_id = Uuid::new_v4();
    let mut tx =
        crate::auth::begin_rls_transaction(&db, user_id, &["onboarding_manager".to_string()])
            .await
            .expect("beginning an RLS transaction must succeed");

    let (company_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
    )
    .bind("Test Upsert Co")
    .fetch_one(&mut *tx)
    .await
    .unwrap();

    let (facility_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.facilities (company_id, name, source) VALUES ($1, $2, 'manual') RETURNING id",
    )
    .bind(company_id)
    .bind("Test Upsert Facility")
    .fetch_one(&mut *tx)
    .await
    .unwrap();

    upsert_person_and_link_to_facility(
        &mut tx,
        facility_id,
        &PersonAssignment {
            full_name: "Irene Chen".to_string(),
            email: Some("irene@example.com".to_string()),
            phone: Some("(301) 555-0100".to_string()),
            role: "owner".to_string(),
        },
        "process_street",
    )
    .await
    .expect("first upsert must succeed");

    // Second call: same email, same name, a fresher phone number.
    upsert_person_and_link_to_facility(
        &mut tx,
        facility_id,
        &PersonAssignment {
            full_name: "Irene Chen".to_string(),
            email: Some("irene@example.com".to_string()),
            phone: Some("(301) 787-9221".to_string()),
            role: "owner".to_string(),
        },
        "process_street",
    )
    .await
    .expect("second upsert must succeed");

    let people: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT p.full_name, p.email::text, p.phone
           FROM clients.facility_people fp
           JOIN clients.people p ON p.id = fp.person_id
          WHERE fp.facility_id = $1",
    )
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await
    .unwrap();

    assert_eq!(
        people.len(),
        1,
        "the same email+name must never produce a second person or a second link"
    );
    assert_eq!(
        people[0].2.as_deref(),
        Some("(301) 787-9221"),
        "phone must be refreshed"
    );

    tx.rollback()
        .await
        .expect("rollback must succeed -- this test writes no real data");
}

/// Proves the real 2026-09-08 Dubuqueland/Soppe bug is fixed: several
/// genuinely distinct people sharing one family inbox (and the same
/// role) must become separate `clients.people` rows, not collapse
/// into one because they share an email. Needs a real, reachable
/// Postgres with every migration applied, same as the golden fixture
/// test above -- `#[ignore]`d for the same reason.
#[tokio::test]
#[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
async fn upsert_person_and_link_to_facility_keeps_distinct_names_separate_on_a_shared_email() {
    let _ = dotenvy::from_filename(".env.local");

    let db = crate::db::connect_test();
    let user_id = Uuid::new_v4();
    let mut tx =
        crate::auth::begin_rls_transaction(&db, user_id, &["onboarding_manager".to_string()])
            .await
            .expect("beginning an RLS transaction must succeed");

    let (company_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
    )
    .bind("Test Shared Inbox Co")
    .fetch_one(&mut *tx)
    .await
    .unwrap();

    let (facility_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.facilities (company_id, name, source) VALUES ($1, $2, 'manual') RETURNING id",
    )
    .bind(company_id)
    .bind("Test Shared Inbox Facility")
    .fetch_one(&mut *tx)
    .await
    .unwrap();

    for full_name in ["Barb Soppe", "Carrie Krueger", "Chad Soppe"] {
        upsert_person_and_link_to_facility(
            &mut tx,
            facility_id,
            &PersonAssignment {
                full_name: full_name.to_string(),
                email: Some("dubuquemini@gmail.com".to_string()),
                phone: Some("563-583-5405".to_string()),
                role: "owner".to_string(),
            },
            "process_street",
        )
        .await
        .unwrap_or_else(|_| panic!("upsert for {full_name} must succeed"));
    }

    let people: Vec<(String,)> = sqlx::query_as(
        "SELECT p.full_name
           FROM clients.facility_people fp
           JOIN clients.people p ON p.id = fp.person_id
          WHERE fp.facility_id = $1
          ORDER BY p.full_name",
    )
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await
    .unwrap();

    assert_eq!(
        people,
        vec![
            ("Barb Soppe".to_string(),),
            ("Carrie Krueger".to_string(),),
            ("Chad Soppe".to_string(),)
        ],
        "three distinct people sharing one email+role must stay three distinct roster rows"
    );

    tx.rollback()
        .await
        .expect("rollback must succeed -- this test writes no real data");
}

/// Proves `heal_person_in_place`'s actual point against real
/// Postgres: it corrects a specific, already-known person's own
/// name/phone by id -- the real self-heal case, Sand-Sto's own
/// "Irene Chen - (301) 787-9221" (a pre-fix dash-format parse glued
/// onto her name). Needs a real, reachable Postgres with every
/// migration applied, same as the golden fixture test above --
/// `#[ignore]`d for the same reason.
#[tokio::test]
#[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
async fn heal_person_in_place_corrects_a_known_persons_own_name_and_phone() {
    let _ = dotenvy::from_filename(".env.local");

    let db = crate::db::connect_test();
    let user_id = Uuid::new_v4();
    let mut tx =
        crate::auth::begin_rls_transaction(&db, user_id, &["onboarding_manager".to_string()])
            .await
            .expect("beginning an RLS transaction must succeed");

    let (company_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
    )
    .bind("Test Heal Co")
    .fetch_one(&mut *tx)
    .await
    .unwrap();

    let (facility_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.facilities (company_id, name, source) VALUES ($1, $2, 'manual') RETURNING id",
    )
    .bind(company_id)
    .bind("Test Heal Facility")
    .fetch_one(&mut *tx)
    .await
    .unwrap();

    // The garbled name, same shape the old dash-format parser bug
    // actually produced for Sand-Sto's own Irene Chen.
    upsert_person_and_link_to_facility(
        &mut tx,
        facility_id,
        &PersonAssignment {
            full_name: "Irene Chen - (301) 787-9221".to_string(),
            email: Some("irene@example.com".to_string()),
            phone: None,
            role: "owner".to_string(),
        },
        "process_street",
    )
    .await
    .expect("initial upsert must succeed");

    let (person_id,): (Uuid,) = sqlx::query_as(
        "SELECT p.id FROM clients.facility_people fp
           JOIN clients.people p ON p.id = fp.person_id
          WHERE fp.facility_id = $1",
    )
    .bind(facility_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();

    heal_person_in_place(&mut tx, person_id, "Irene Chen", Some("(301) 787-9221"))
        .await
        .expect("heal must succeed");

    let people: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT p.full_name, p.email::text, p.phone
           FROM clients.facility_people fp
           JOIN clients.people p ON p.id = fp.person_id
          WHERE fp.facility_id = $1",
    )
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await
    .unwrap();

    assert_eq!(
        people.len(),
        1,
        "healing in place must never create a second person or a second link"
    );
    assert_eq!(
        people[0].0, "Irene Chen",
        "the stale, garbled name must be corrected"
    );
    assert_eq!(people[0].2.as_deref(), Some("(301) 787-9221"));

    tx.rollback()
        .await
        .expect("rollback must succeed -- this test writes no real data");
}

/// Proves `policy_delinquency_entries`' own CHECK constraint against
/// real Postgres, not just `api::clients_facility_policies_edit`'s
/// mirrored application-level validation -- a `trigger_type` of
/// `paid_through_date` must reject a `trigger_category`, and
/// `step_category` must require one. Needs a real, reachable
/// Postgres with every migration applied -- `#[ignore]`d for the
/// same reason the other live tests in this module are.
#[tokio::test]
#[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
async fn policy_delinquency_entries_trigger_check_matches_the_apps_own_validation() {
    let _ = dotenvy::from_filename(".env.local");
    let db = crate::db::connect_test();
    let mut tx = crate::auth::begin_rls_transaction(
        &db,
        Uuid::new_v4(),
        &["onboarding_manager".to_string()],
    )
    .await
    .unwrap();

    let (company_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
    )
    .bind("Test Trigger Check Co")
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let (facility_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.facilities (company_id, name, source) VALUES ($1, $2, 'manual') RETURNING id",
    )
    .bind(company_id)
    .bind("Test Trigger Check Facility")
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query("INSERT INTO clients.facility_policies (facility_id) VALUES ($1)")
        .bind(facility_id)
        .execute(&mut *tx)
        .await
        .unwrap();

    // Valid: paid_through_date with no trigger_category.
    sqlx::query(
        "INSERT INTO clients.policy_delinquency_entries \
         (facility_policies_id, category, name, amount, trigger_type, sort_order) \
         VALUES ($1, 'pre_lien', 'Pre-Lien Fee', 0, 'paid_through_date', 1)",
    )
    .bind(facility_id)
    .execute(&mut *tx)
    .await
    .expect("paid_through_date with no trigger_category must be accepted");

    // Valid: step_category with a real trigger_category set.
    sqlx::query(
        "INSERT INTO clients.policy_delinquency_entries \
         (facility_policies_id, category, name, amount, trigger_type, trigger_category, sort_order) \
         VALUES ($1, 'lien', 'Lien Fee', 25, 'step_category', 'pre_lien', 2)",
    )
    .bind(facility_id)
    .execute(&mut *tx)
    .await
    .expect("step_category with a trigger_category must be accepted");

    // Invalid: paid_through_date with a trigger_category set anyway.
    let rejected = sqlx::query(
        "INSERT INTO clients.policy_delinquency_entries \
         (facility_policies_id, category, name, amount, trigger_type, trigger_category, sort_order) \
         VALUES ($1, 'cut_lock', 'Cut Lock Fee', 10, 'paid_through_date', 'pre_lien', 3)",
    )
    .bind(facility_id)
    .execute(&mut *tx)
    .await;
    assert!(
        rejected.is_err(),
        "paid_through_date with a trigger_category must be rejected by the CHECK"
    );

    tx.rollback()
        .await
        .expect("rollback must succeed -- this test writes no real data");
}

/// Proves `edit_person_and_facility_link`'s actual point against
/// real Postgres: editing a 'process_street' person with
/// `protect_from_resync = true` flips their link's source to
/// 'manual' (permanently exempting them from the Users tab's own
/// self-heal pass, `api::clients_facility_people::get_facility_people`'s
/// own doc comment) -- without it, the edit stays 'process_street'.
/// Needs a real, reachable Postgres with every migration applied --
/// `#[ignore]`d for the same reason the other live tests here are.
#[tokio::test]
#[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
async fn edit_person_and_facility_link_flips_source_to_manual_only_when_protected() {
    let _ = dotenvy::from_filename(".env.local");
    let db = crate::db::connect_test();
    let mut tx = crate::auth::begin_rls_transaction(
        &db,
        Uuid::new_v4(),
        &["onboarding_manager".to_string()],
    )
    .await
    .unwrap();

    let (company_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
    )
    .bind("Test Edit Person Co")
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let (facility_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.facilities (company_id, name, source) VALUES ($1, $2, 'manual') RETURNING id",
    )
    .bind(company_id)
    .bind("Test Edit Person Facility")
    .fetch_one(&mut *tx)
    .await
    .unwrap();

    let assignment = PersonAssignment {
        full_name: "Kyle Lindley".to_string(),
        email: Some("kyle@example.com".to_string()),
        phone: Some("630-650-0137".to_string()),
        role: "owner".to_string(),
    };
    upsert_person_and_link_to_facility(&mut tx, facility_id, &assignment, "process_street")
        .await
        .unwrap();

    let (person_id,): (Uuid,) =
        sqlx::query_as("SELECT id FROM clients.people WHERE email = 'kyle@example.com'")
            .fetch_one(&mut *tx)
            .await
            .unwrap();

    // Edit without protecting -- stays 'process_street'.
    edit_person_and_facility_link(
        &mut tx,
        facility_id,
        person_id,
        "owner",
        "Kyle W. Lindley",
        Some("kyle@example.com"),
        Some("630-650-0137"),
        "owner",
        false,
    )
    .await
    .unwrap();

    let (source_after_unprotected_edit,): (String,) = sqlx::query_as(
        "SELECT source FROM clients.facility_people WHERE facility_id = $1 AND person_id = $2 AND role = 'owner'",
    )
    .bind(facility_id)
    .bind(person_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(source_after_unprotected_edit, "process_street");

    // Edit with protecting -- flips to 'manual', permanently.
    edit_person_and_facility_link(
        &mut tx,
        facility_id,
        person_id,
        "owner",
        "Kyle W. Lindley",
        Some("kyle@example.com"),
        Some("630-650-0137"),
        "owner",
        true,
    )
    .await
    .unwrap();

    let (source_after_protected_edit,): (String,) = sqlx::query_as(
        "SELECT source FROM clients.facility_people WHERE facility_id = $1 AND person_id = $2 AND role = 'owner'",
    )
    .bind(facility_id)
    .bind(person_id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(source_after_protected_edit, "manual");

    tx.rollback()
        .await
        .expect("rollback must succeed -- this test writes no real data");
}
