use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::auth::begin_rls_transaction;
use crate::client_ops::audit_log;
use crate::clients::intake_mapping::map_intake_fields;
use crate::clients::known_workflows::{
    CONTRACT_ORDER_WORKFLOW_ID, INTAKE_WORKFLOW_ID, MERCHANT_ACCOUNT_WORKFLOW_ID,
};
use crate::clients::person_index::{
    extract_contract_order_people, extract_intake_people, extract_merchant_account_people,
    ExtractedPerson,
};
use crate::process_street::ProcessStreetClient;

use super::progress::{SyncError, SyncProgressHandle, SyncState, SyncStats, try_claim_running};
use super::refresh::{refresh_matching_company, refresh_matching_facility};

/// See this module's own doc comment for why a fixed, non-empty
/// placeholder is correct here, and why it also covers the write
/// policies `client_ops::vendor_format`'s read-only use of the same
/// pattern never had to.
const SYSTEM_USER_ID: Uuid = Uuid::nil();
const SYSTEM_ROLE: &str = "onboarding_manager";

/// Fallback only -- used when `client_ops.process_street_settings`
/// can't be read at all (a transient DB error), never as the normal
/// path. The settings row itself defaults to the same value.
fn default_sync_interval_hours() -> i16 {
    24
}

/// Pure decision at the heart of the delta check -- pulled out of the
/// DB/network-heavy loop below so it has its own direct unit tests, no
/// fixture or live call needed. `None` (never synced before) always
/// needs a refresh; otherwise a run only needs one when PS's own
/// `updatedDate` has moved past what was last recorded.
fn needs_refresh(previously_synced_at: Option<DateTime<Utc>>, current_updated_at: DateTime<Utc>) -> bool {
    match previously_synced_at {
        Some(prev) => prev < current_updated_at,
        None => true,
    }
}

/// What one run's own delta-refresh actually did, beyond the
/// `ps_person_index` bookkeeping `sync_one_run` always performs when a
/// refresh is needed at all -- whether an already-imported Company and/or
/// Facility record (see `company_refreshed`/`facility_refreshed`) also
/// got its fields updated from this same fresh fetch. A single Intake
/// run can match both: since `create.rs`'s 2026-09-02 change, a
/// company's source run is often also one of its own facility runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RunSyncOutcome {
    person_index_refreshed: bool,
    people_indexed: usize,
    company_refreshed: bool,
    facility_refreshed: bool,
}

type ExtractFn = fn(&[crate::process_street::FormField]) -> Vec<ExtractedPerson>;

const WORKFLOWS: &[(&str, &str, ExtractFn)] = &[
    (INTAKE_WORKFLOW_ID, "intake", extract_intake_people),
    (MERCHANT_ACCOUNT_WORKFLOW_ID, "merchant_account", extract_merchant_account_people),
    (CONTRACT_ORDER_WORKFLOW_ID, "contract_order", extract_contract_order_people),
];

/// Applies the delta check to exactly one run, refreshing it (deleting
/// and re-inserting its `ps_person_index` rows, upserting
/// `ps_sync_state`, and -- for an Intake run only -- refreshing any
/// already-imported Company/Facility whose own `ps_intake_run_id`
/// matches, see `refresh_matching_company`/`refresh_matching_facility`)
/// only when `needs_refresh` says so. Split out of `sync_workflow_within`
/// so `live_tests` can prove the skip behavior against one specific
/// known run without paying for a real `/form-fields` fetch on every
/// other real run in the workflow -- the expensive call this whole
/// module exists to avoid making unnecessarily.
///
/// Company/Facility refresh is Intake-only for now: a company's fields
/// are seeded from whichever facility's own Intake run answered "first
/// time = Yes" (see `clients.companies.ps_intake_run_id`'s own migration
/// comment), not from a persisted link to a Merchant Account run -- there
/// is no such link stored today, so a later change to that Merchant
/// Account run's own data has nothing to refresh against yet.
async fn sync_one_run(
    tx: &mut Transaction<'_, Postgres>,
    client: &ProcessStreetClient,
    workflow_key: &'static str,
    run: &crate::process_street::WorkflowRun,
    previously_synced_at: Option<DateTime<Utc>>,
    extract: ExtractFn,
) -> Result<RunSyncOutcome, SyncError> {
    if !needs_refresh(previously_synced_at, run.updated_at()) {
        return Ok(RunSyncOutcome::default());
    }

    let fields = client.get_run_form_fields(&run.id).await?;
    let people = extract(&fields);

    sqlx::query("DELETE FROM clients.ps_person_index WHERE workflow = $1 AND ps_run_id = $2")
        .bind(workflow_key)
        .bind(&run.id)
        .execute(&mut **tx)
        .await?;

    for person in &people {
        sqlx::query(
            "INSERT INTO clients.ps_person_index
                 (workflow, ps_run_id, run_name, full_name, email, phone, role)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(workflow_key)
        .bind(&run.id)
        .bind(&run.name)
        .bind(&person.full_name)
        .bind(&person.email)
        .bind(&person.phone)
        .bind(person.role)
        .execute(&mut **tx)
        .await?;
    }

    sqlx::query(
        "INSERT INTO clients.ps_sync_state (workflow, ps_run_id, run_name, ps_updated_at, last_synced_at)
         VALUES ($1, $2, $3, $4, now())
         ON CONFLICT (workflow, ps_run_id) DO UPDATE SET
             run_name = EXCLUDED.run_name,
             ps_updated_at = EXCLUDED.ps_updated_at,
             last_synced_at = now()",
    )
    .bind(workflow_key)
    .bind(&run.id)
    .bind(&run.name)
    .bind(run.updated_at())
    .execute(&mut **tx)
    .await?;

    let (company_refreshed, facility_refreshed) = if workflow_key == "intake" {
        let mapped = map_intake_fields(&fields);
        let company_refreshed = refresh_matching_company(tx, &run.id, &mapped.company).await?;
        let facility_refreshed = refresh_matching_facility(tx, &run.id, &mapped.facility).await?;
        (company_refreshed, facility_refreshed)
    } else {
        (false, false)
    };

    Ok(RunSyncOutcome {
        person_index_refreshed: true,
        people_indexed: people.len(),
        company_refreshed,
        facility_refreshed,
    })
}

/// Syncs a pre-fetched list of one workflow's runs into
/// `ps_sync_state`/`ps_person_index` within an already-open transaction
/// -- the caller decides whether to commit or roll back, same
/// discipline `clients::ingest::ingest_facility` and every
/// `clients::repository` function already use. Takes `runs` rather than
/// a `workflow_id` and listing them itself so `run_all_workflows_with_progress`
/// can list every workflow up front (to know the real total before
/// processing any of them) without a second, redundant list call here.
///
/// `on_processed` fires once per run after its delta check resolves --
/// `run_all_workflows_with_progress` uses it to advance a shared
/// progress counter; the plain `sync_workflow` entry point below passes
/// a no-op.
async fn sync_runs_within(
    tx: &mut Transaction<'_, Postgres>,
    client: &ProcessStreetClient,
    workflow_key: &'static str,
    runs: &[crate::process_street::WorkflowRun],
    extract: ExtractFn,
    mut on_processed: impl FnMut(),
) -> Result<SyncStats, SyncError> {
    let existing: HashMap<String, DateTime<Utc>> = sqlx::query_as(
        "SELECT ps_run_id, ps_updated_at FROM clients.ps_sync_state WHERE workflow = $1",
    )
    .bind(workflow_key)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();

    let mut runs_changed = 0;
    let mut people_indexed = 0;
    let mut companies_refreshed = 0;
    let mut facilities_refreshed = 0;

    for run in runs {
        let outcome = sync_one_run(
            tx,
            client,
            workflow_key,
            run,
            existing.get(&run.id).copied(),
            extract,
        )
        .await?;
        if outcome.person_index_refreshed {
            runs_changed += 1;
            people_indexed += outcome.people_indexed;
        }
        if outcome.company_refreshed {
            companies_refreshed += 1;
        }
        if outcome.facility_refreshed {
            facilities_refreshed += 1;
        }
        on_processed();
    }

    Ok(SyncStats {
        workflow: workflow_key,
        runs_seen: runs.len(),
        runs_changed,
        people_indexed,
        companies_refreshed,
        facilities_refreshed,
    })
}

/// The entry point both the nightly timer and the manual "Sync Now"
/// endpoint use -- lists every workflow's runs up front so `progress.total_runs`
/// is known (and
/// therefore a meaningful percentage is showable) before any per-run
/// work starts, and stops at the first error rather than continuing
/// past it (a partial percentage that then silently stalls is worse
/// here than a clearly-`Failed` state the UI can show).
///
/// Callers must hold the claim from `try_claim_running` before calling
/// this -- it sets `Completed`/`Failed` on every exit path but does not
/// itself guard against two concurrent invocations.
///
/// `actor_user_id` is who to credit in the Activity Log for this run --
/// `SYSTEM_USER_ID` for the nightly timer, the real caller's id for the
/// manual "Sync Now"/scoped Re-sync triggers -- recorded on both
/// `SYNC_COMPLETED` and `SYNC_FAILED` (see `client_ops::audit_log`'s own
/// module doc on why a failed Process Street call belongs in the same
/// trail as every other activity, not just server logs).
pub async fn run_all_workflows_with_progress(
    client: &ProcessStreetClient,
    db: &PgPool,
    progress: &SyncProgressHandle,
    actor_user_id: Uuid,
) {
    let mut per_workflow_runs = Vec::with_capacity(WORKFLOWS.len());
    for (workflow_id, workflow_key, extract) in WORKFLOWS {
        match client.list_workflow_runs(workflow_id).await {
            Ok(runs) => per_workflow_runs.push((*workflow_key, *extract, runs)),
            Err(err) => {
                fail(db, progress, actor_user_id, err.to_string()).await;
                return;
            }
        }
    }

    let total_runs: usize = per_workflow_runs.iter().map(|(_, _, runs)| runs.len()).sum();
    progress.write().total_runs = total_runs;

    let mut results = Vec::with_capacity(per_workflow_runs.len());

    for (workflow_key, extract, runs) in &per_workflow_runs {
        let mut tx = match begin_rls_transaction(db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()]).await {
            Ok(tx) => tx,
            Err(err) => {
                fail(db, progress, actor_user_id, err.to_string()).await;
                return;
            }
        };

        let stats_result = sync_runs_within(&mut tx, client, workflow_key, runs, *extract, || {
            progress.write().processed_runs += 1;
        })
        .await;

        let stats = match stats_result {
            Ok(stats) => stats,
            Err(err) => {
                let _ = tx.rollback().await;
                fail(db, progress, actor_user_id, err.to_string()).await;
                return;
            }
        };

        if let Err(err) = tx.commit().await {
            fail(db, progress, actor_user_id, err.to_string()).await;
            return;
        }

        results.push(stats);
    }

    let companies_refreshed: usize = results.iter().map(|s| s.companies_refreshed).sum();
    let facilities_refreshed: usize = results.iter().map(|s| s.facilities_refreshed).sum();

    audit_log::record(
        db,
        audit_log::event::SYNC_COMPLETED,
        actor_user_id,
        "sync_run",
        None,
        audit_log::Change::none(),
        None,
        None,
        serde_json::json!({
            "total_runs": total_runs,
            "companies_refreshed": companies_refreshed,
            "facilities_refreshed": facilities_refreshed,
            "results": results.iter().map(|s| serde_json::json!({
                "workflow": s.workflow,
                "runs_seen": s.runs_seen,
                "runs_changed": s.runs_changed,
            })).collect::<Vec<_>>(),
        }),
    )
    .await;

    let mut guard = progress.write();
    guard.state = SyncState::Completed;
    guard.results = results;
}

/// Marks the shared progress handle `Failed` and records `SYNC_FAILED`
/// in the Activity Log -- e.g. Process Street was unreachable or
/// returned an error partway through. See this module's own doc comment
/// on why a sync failure is worth its own audited event, not just a
/// server-log line.
async fn fail(db: &PgPool, progress: &SyncProgressHandle, actor_user_id: Uuid, message: String) {
    audit_log::record(
        db,
        audit_log::event::SYNC_FAILED,
        actor_user_id,
        "sync_run",
        None,
        audit_log::Change::none(),
        None,
        None,
        serde_json::json!({ "error": message }),
    )
    .await;

    let mut guard = progress.write();
    guard.state = SyncState::Failed;
    guard.error = Some(message);
}

/// Reads `client_ops.process_street_settings.sync_interval_hours` on the
/// same system role/RLS pattern as everything else in this module. Falls
/// back to `default_sync_interval_hours()` (never a panic, never
/// blocking the loop forever) on any read failure -- a transient DB
/// hiccup should delay this cycle's sync, not crash the background task.
async fn fetch_sync_interval_hours(db: &PgPool) -> i16 {
    let result: Result<(i16,), sqlx::Error> = async {
        let mut tx = begin_rls_transaction(db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()]).await?;
        let row = sqlx::query_as(
            "SELECT sync_interval_hours FROM client_ops.process_street_settings WHERE id = 1",
        )
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }
    .await;

    match result {
        Ok((interval_hours,)) => interval_hours,
        Err(err) => {
            tracing::error!(
                error = %err,
                "failed to read the configured Process Street sync interval; defaulting to 24h for this cycle"
            );
            default_sync_interval_hours()
        }
    }
}

/// Sleeps for the currently configured interval before the next sync
/// tick -- re-read on every call, not cached, so a settings change
/// (`api::process_street_settings`) takes effect on the very next cycle
/// without needing a server restart. Unlike the old fixed-time-of-day
/// schedule this replaces, there is no "next occurrence" to compute:
/// every tick is simply "interval hours after the last one finished",
/// which is exactly what sleeping this long, then looping, already does
/// -- see `clients::sync`'s own module doc for why a much shorter
/// interval than the old once-daily default is now realistic at all
/// (the delta mechanism makes an unchanged run essentially free).
async fn sleep_until_next_scheduled_sync(db: &PgPool) {
    let interval_hours = fetch_sync_interval_hours(db).await;
    let sleep_duration = std::time::Duration::from_secs((interval_hours.max(1) as u64) * 3600);

    tracing::info!(
        next_sync_at = %(Utc::now() + ChronoDuration::hours(i64::from(interval_hours))),
        interval_hours,
        "Process Street sync scheduled"
    );

    tokio::time::sleep(sleep_duration).await;
}

/// Spawns the scheduled sync loop -- runs every configured
/// `sync_interval_hours` (`client_ops.process_street_settings`, default
/// 24) or when `api::clients_sync::start_sync` triggers one manually.
/// Deliberately does NOT also fire immediately on startup the
/// way `client_ops::vendor_format::start_refresh_task` does -- that
/// task's cache would otherwise sit empty until the first tick and
/// block real request handling; an empty `ps_person_index` just means
/// fewer search results, and firing on every restart would mean every
/// local dev run or every prod deploy kicks off a real, resource-
/// competing sync against the live PS API whether anyone wants one
/// right then or not (confirmed directly, 2026-08-31: a restart-
/// triggered first sync measurably slowed down an unrelated live test
/// running against the same dev database at the same time).
///
/// Shares `progress` with the manual "Sync Now" endpoint
/// (`api::clients_sync`) -- `try_claim_running` means a scheduled run
/// that lands while a manual sync is still in flight (or vice versa)
/// just skips rather than starting a second, overlapping pass.
pub fn start_background_sync_task(
    client: Arc<ProcessStreetClient>,
    db: PgPool,
    progress: SyncProgressHandle,
) {
    tokio::spawn(async move {
        loop {
            sleep_until_next_scheduled_sync(&db).await;

            if !try_claim_running(&progress) {
                tracing::warn!(
                    "Skipping this Process Street sync tick -- a sync (manual or scheduled) is already running"
                );
                continue;
            }

            run_all_workflows_with_progress(&client, &db, &progress, SYSTEM_USER_ID).await;

            let finished = progress.read().clone();
            match finished.state {
                SyncState::Completed => tracing::info!(
                    runs_seen = finished.total_runs,
                    results = ?finished.results,
                    "Process Street person-index sync completed"
                ),
                SyncState::Failed => tracing::error!(
                    error = finished.error.as_deref().unwrap_or("unknown error"),
                    "Process Street person-index sync failed"
                ),
                SyncState::Idle | SyncState::Running => {
                    // Unreachable in practice -- run_all_workflows_with_progress
                    // always leaves Completed or Failed on every exit path.
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_synced_before_always_needs_refresh() {
        assert!(needs_refresh(None, Utc::now()));
    }

    #[test]
    fn unchanged_updated_at_does_not_need_refresh() {
        let t = Utc::now();
        assert!(!needs_refresh(Some(t), t));
    }

    #[test]
    fn a_later_updated_at_needs_refresh() {
        let earlier = Utc::now() - ChronoDuration::days(1);
        let later = Utc::now();
        assert!(needs_refresh(Some(earlier), later));
    }

    #[test]
    fn an_updated_at_that_moved_backward_does_not_need_refresh() {
        // Should never happen against the real API, but the comparison
        // itself must not treat "earlier than what's recorded" as a
        // reason to refresh -- only strictly-later does.
        let later = Utc::now();
        let earlier = later - ChronoDuration::days(1);
        assert!(!needs_refresh(Some(later), earlier));
    }
}

#[cfg(test)]
mod live_tests {
    use serial_test::serial;

    use crate::process_street::{ProcessStreetClient, ProcessStreetConfig};

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

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let mut tx = begin_rls_transaction(&db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()])
            .await
            .expect("beginning an RLS transaction must succeed");

        let first_outcome = sync_one_run(&mut tx, &client, "intake", &run, None, extract_intake_people)
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

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
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
        assert!(refreshed, "the fresh name should differ from the seeded state and trigger an update");

        let (phone, name): (Option<String>, String) =
            sqlx::query_as("SELECT phone, name FROM clients.facilities WHERE id = $1")
                .bind(facility_id)
                .fetch_one(&mut *tx)
                .await
                .expect("re-reading the facility must succeed");

        assert_eq!(phone.as_deref(), Some("MANUALLY-CORRECTED"), "a protected field must survive a refresh");
        assert_eq!(name, "Highway 20 Self Storage", "an unprotected field must take the fresh PS value");

        tx.rollback()
            .await
            .expect("rollback must succeed -- this is a one-time check, must not persist against the real row");
    }
}
