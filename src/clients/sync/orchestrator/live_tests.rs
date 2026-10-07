use serial_test::serial;

use crate::process_street::ProcessStreetConfig;

use uuid::Uuid;

use crate::auth::begin_rls_transaction;
use crate::clients::intake_mapping::map_intake_fields;
use crate::clients::known_workflows::{INTAKE_WORKFLOW_ID, MERCHANT_ACCOUNT_WORKFLOW_ID};
use crate::clients::person_index::{extract_intake_people, extract_merchant_account_people};
use crate::process_street::ProcessStreetClient;

use super::super::refresh::refresh_matching_facility;
use super::*;

/// Proves the delta-sync pipeline end to end against the real PS
/// API and a real, migrated Postgres, scoped to exactly one known
/// run (Highway 20's Intake run) rather than a whole workflow --
/// looked up via the cheap `search_workflow_runs_by_name` list call,
/// so this test's only expensive `/form-fields` fetch is the single
/// one the first sync pass legitimately needs, not one per every
/// real Intake run in the org.
///
/// First pass: never-synced-before, so it must refresh and index
/// real people. Second pass, against the very same run object (same
/// `updated_at`, unless someone edits it in PS in the few
/// milliseconds between the two calls in this test): must skip
/// entirely -- the actual delta behavior this module exists for.
/// Both run inside one uncommitted transaction, rolled back at the
/// end so nothing persists.
///
/// `#[ignore]`d for the same reason every other live test in this
/// crate is: needs a real, reachable Postgres AND a real
/// `PROCESS_STREET_API_KEY`. Run explicitly with
/// `cargo test -- --ignored sync_one_run_indexes_a_real_run_and_skips_an_unchanged_one`.
#[tokio::test]
#[ignore = "needs a real, reachable Postgres AND a real Process Street API key -- see doc comment"]
#[serial(client_pii_encryption_key_env)]
async fn sync_one_run_indexes_a_real_run_and_skips_an_unchanged_one() {
    let _ = dotenvy::from_filename(".env.local");

    let ps_config =
        ProcessStreetConfig::from_env().expect("PROCESS_STREET_API_KEY must be set in .env.local");
    let client = ProcessStreetClient::new(ps_config);

    let matches = client
        .search_workflow_runs_by_name(INTAKE_WORKFLOW_ID, "highway")
        .await
        .expect("search must succeed against the live API");
    let run = matches
        .into_iter()
        .find(|r| r.name == "Highway 20 Self Storage - QMS Onboarding")
        .expect("Highway 20's Intake run must be found");

    let db = crate::db::connect_test();
    let mut tx = begin_rls_transaction(&db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()])
        .await
        .expect("beginning an RLS transaction must succeed");

    let first_outcome = sync_one_run(
        &mut tx,
        &client,
        "intake",
        &run,
        None,
        extract_intake_people,
    )
    .await
    .expect("first sync pass must succeed against the live API");

    assert!(
        first_outcome.person_index_refreshed,
        "a never-synced-before run must always refresh"
    );
    assert!(
        first_outcome.people_indexed > 0,
        "at least one real Owner/DM/Manager person must have been indexed"
    );

    let (indexed_count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM clients.ps_person_index WHERE workflow = 'intake' AND ps_run_id = $1",
    )
    .bind(&run.id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert!(indexed_count > 0);

    // Second pass against the same run, now passing its own
    // just-recorded `updated_at` as `previously_synced_at` -- must
    // be skipped, the actual delta behavior this module exists for.
    let second_outcome = sync_one_run(
        &mut tx,
        &client,
        "intake",
        &run,
        Some(run.updated_at()),
        extract_intake_people,
    )
    .await
    .expect("second sync pass must succeed");

    assert!(
        !second_outcome.person_index_refreshed,
        "an unchanged run must not need re-fetching"
    );
    assert_eq!(second_outcome.people_indexed, 0);

    tx.rollback()
        .await
        .expect("rollback must succeed -- this is a one-time check, not a real sync");
}

/// Proves `business_dba` extraction (added 2026-09-17, see
/// `merchant_account_correlation.rs`'s own doc comment) actually
/// persists a real value, against Highway 20's own real, already-
/// linked Merchant Account run -- confirmed elsewhere this session
/// to answer `Business_DBA: "Highway 20 self storage"`.
#[tokio::test]
#[ignore = "needs a real, reachable Postgres AND a real Process Street API key -- see doc comment"]
#[serial(client_pii_encryption_key_env)]
async fn sync_one_run_persists_a_real_business_dba_for_a_merchant_account_run() {
    let _ = dotenvy::from_filename(".env.local");

    let ps_config =
        ProcessStreetConfig::from_env().expect("PROCESS_STREET_API_KEY must be set in .env.local");
    let client = ProcessStreetClient::new(ps_config);

    let matches = client
        .search_workflow_runs_by_name(MERCHANT_ACCOUNT_WORKFLOW_ID, "highway 20")
        .await
        .expect("search must succeed against the live API");
    let run = matches
        .into_iter()
        .find(|r| r.name == "Prairie Enterprises (Highway 20)")
        .expect("Highway 20's own Merchant Account run must be found");

    let db = crate::db::connect_test();
    let mut tx = begin_rls_transaction(&db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()])
        .await
        .expect("beginning an RLS transaction must succeed");

    sync_one_run(
        &mut tx,
        &client,
        "merchant_account",
        &run,
        None,
        extract_merchant_account_people,
    )
    .await
    .expect("sync pass must succeed against the live API");

    let (business_dba,): (Option<String>,) = sqlx::query_as(
        "SELECT business_dba FROM clients.ps_sync_state WHERE workflow = 'merchant_account' AND ps_run_id = $1",
    )
    .bind(&run.id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();

    assert_eq!(business_dba.as_deref(), Some("Highway 20 self storage"));

    tx.rollback()
        .await
        .expect("rollback must succeed -- this is a one-time check, not a real sync");
}

/// Proves `refresh_matching_facility`'s hand-written UPDATE is
/// actually valid against the real, migrated schema -- nothing about
/// this module's plain dynamic SQL is checked at compile time (see
/// `clients::repository`'s own doc comment on why), so a column-name
/// typo in that statement would otherwise only ever surface the
/// first time a real sync tick found something to refresh.
///
/// Uses Highway 20's real, already-imported facility row (created
/// during this same 2026-09-02 session's own live testing of
/// `clients::create`) rather than inserting a fixture row: marks its
/// `phone` as manually edited with an obviously-fake value, then
/// refreshes from the real live run. Asserts the protected `phone`
/// survived untouched while `name` (not protected) took the fresh
/// value. Rolled back so this doesn't actually clobber the real row.
#[tokio::test]
#[ignore = "needs a real, reachable Postgres AND a real Process Street API key -- see doc comment"]
#[serial(client_pii_encryption_key_env)]
async fn refresh_matching_facility_updates_unprotected_fields_and_skips_protected_ones() {
    let _ = dotenvy::from_filename(".env.local");

    let ps_config =
        ProcessStreetConfig::from_env().expect("PROCESS_STREET_API_KEY must be set in .env.local");
    let client = ProcessStreetClient::new(ps_config);

    let db = crate::db::connect_test();
    let mut tx = begin_rls_transaction(&db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()])
        .await
        .expect("beginning an RLS transaction must succeed");

    // Highway 20's real Intake run id -- see this module's own live
    // tests above for the same constant used to find it via search.
    let run_id = "iy22NyiqGjwAAytKp0NErQ";

    let existing_id: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM clients.facilities WHERE ps_intake_run_id = $1")
            .bind(run_id)
            .fetch_optional(&mut *tx)
            .await
            .expect("facility lookup must succeed");
    let Some((facility_id,)) = existing_id else {
        tx.rollback().await.expect("rollback must succeed");
        panic!("Highway 20's facility row must already exist -- run clients::create's own live test first, or import it via the app");
    };

    // phone is seeded fake AND protected (must survive); name is
    // seeded stale but NOT protected (must be corrected back to the
    // real value) -- without a genuinely stale unprotected field,
    // the refreshed struct would equal current exactly and
    // `refresh_matching_facility` would correctly report no update
    // needed, proving nothing about the UPDATE statement itself.
    sqlx::query(
        "UPDATE clients.facilities SET phone = 'MANUALLY-CORRECTED', name = 'Stale Seeded Name', \
         manually_edited_fields = '{phone}' WHERE id = $1",
    )
    .bind(facility_id)
    .execute(&mut *tx)
    .await
    .expect("seeding the manually-edited phone and stale name must succeed");

    let fields = client
        .get_run_form_fields(run_id)
        .await
        .expect("fetching Highway 20's real fields must succeed");
    let mapped = map_intake_fields(&fields);

    let refreshed = refresh_matching_facility(&mut tx, run_id, &mapped.facility)
        .await
        .expect("refresh must succeed against the real schema");
    assert!(
        refreshed,
        "the fresh name should differ from the seeded state and trigger an update"
    );

    let (phone, name): (Option<String>, String) =
        sqlx::query_as("SELECT phone, name FROM clients.facilities WHERE id = $1")
            .bind(facility_id)
            .fetch_one(&mut *tx)
            .await
            .expect("re-reading the facility must succeed");

    assert_eq!(
        phone.as_deref(),
        Some("MANUALLY-CORRECTED"),
        "a protected field must survive a refresh"
    );
    assert_eq!(
        name, "Highway 20 Self Storage",
        "an unprotected field must take the fresh PS value"
    );

    tx.rollback().await.expect(
        "rollback must succeed -- this is a one-time check, must not persist against the real row",
    );
}
