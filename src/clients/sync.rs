//! Delta-aware background sync feeding `clients.ps_person_index` --
//! Phase 2's harder half (person-name search). Wired into `main.rs`:
//! `start_background_sync_task` runs whenever `PROCESS_STREET_API_KEY`
//! is configured, alongside `api::clients_search`, which reads what this
//! writes. Also proven directly against the real API and real Postgres
//! by `live_tests::sync_one_run_indexes_a_real_run_and_skips_an_unchanged_one`,
//! the same "prove it, then roll back" discipline `clients::ingest`'s
//! own live test uses.
//!
//! **The delta mechanism**: `list_workflow_runs` is cheap (one paginated
//! list call, no per-run fetch) and every run PS returns carries its own
//! `audit.updatedDate` for free. A run's `form-fields` -- the expensive
//! per-run fetch this module exists to avoid doing unnecessarily -- is
//! only re-fetched when that timestamp has moved past what
//! `clients.ps_sync_state` last recorded for it. `updatedDate` only
//! changes when someone actually edits that run in PS, so a facility
//! whose Intake run nobody has touched since the last sync costs
//! nothing beyond the one shared list call.
//!
//! **RLS**: this task has no real authenticated caller (it runs on a
//! timer, not behind a request) -- same situation
//! `client_ops::vendor_format::start_refresh_task` already solves. Its
//! own `SYSTEM_USER_ID` placeholder works because the relevant SELECT
//! policy only checks that `app.current_user_id` is set, not that it
//! names a real user; this module's writes need one step further, since
//! `clients` schema INSERT/UPDATE/DELETE policies also require
//! `onboarding_manager`/`department_manager` -- but `begin_rls_transaction`
//! never validates `role_keys` against a real roles table, it just sets
//! them as the `app.current_user_roles` GUC verbatim (see
//! `clients::ingest`'s own live test), so passing that role list
//! directly satisfies the write policies too. There is no distinct
//! "system" role in this app's RBAC, so reusing the same client-ops
//! write gate every human write already goes through is the pragmatic
//! choice over inventing a new one for this one caller.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration as ChronoDuration, NaiveTime, Utc};
use parking_lot::RwLock;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::auth::begin_rls_transaction;
use crate::clients::known_workflows::{
    CONTRACT_ORDER_WORKFLOW_ID, INTAKE_WORKFLOW_ID, MERCHANT_ACCOUNT_WORKFLOW_ID,
};
use crate::clients::person_index::{
    extract_contract_order_people, extract_intake_people, extract_merchant_account_people,
    ExtractedPerson,
};
use crate::process_street::{ProcessStreetClient, ProcessStreetError};

/// See this module's own doc comment for why a fixed, non-empty
/// placeholder is correct here, and why it also covers the write
/// policies `client_ops::vendor_format`'s read-only use of the same
/// pattern never had to.
const SYSTEM_USER_ID: Uuid = Uuid::nil();
const SYSTEM_ROLE: &str = "onboarding_manager";

/// Fallback only -- used when `client_ops.process_street_settings`
/// can't be read at all (a transient DB error), never as the normal
/// path. The settings row itself defaults to the same value.
fn default_sync_time() -> NaiveTime {
    NaiveTime::from_hms_opt(0, 0, 0).expect("00:00:00 is always a valid time")
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("Process Street request failed: {0}")]
    ProcessStreet(#[from] ProcessStreetError),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncStats {
    pub workflow: &'static str,
    pub runs_seen: usize,
    pub runs_changed: usize,
    pub people_indexed: usize,
}

/// Shared, pollable progress for whichever sync run is currently in
/// flight (or last finished) -- lets `api::clients_sync`'s "Sync Now"
/// button show a live percentage instead of a bare spinner, and lets the
/// manual trigger and the nightly timer share one "is a sync already
/// running" guard (see `try_claim_running`) so a click during the
/// nightly window doesn't start a second, overlapping pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncState {
    #[default]
    Idle,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Default)]
pub struct SyncProgress {
    pub state: SyncState,
    /// Known only once every workflow's (cheap) run list has come back
    /// -- see `run_all_workflows_with_progress`. Zero while state is
    /// still `Idle`.
    pub total_runs: usize,
    /// Incremented once per run after its delta check resolves, whether
    /// that run was actually refreshed or skipped -- "processed" means
    /// "a decision was made," not "a `form-fields` fetch happened."
    pub processed_runs: usize,
    pub results: Vec<SyncStats>,
    /// Set only when `state == Failed` -- the error that stopped the
    /// run early. A failure on one workflow does not appear here; see
    /// `sync_all_workflows`'s own per-workflow error handling for that
    /// (unrelated) case, still used by the plain, progress-free path.
    pub error: Option<String>,
}

impl SyncProgress {
    pub fn percent(&self) -> u8 {
        if self.total_runs == 0 {
            return 0;
        }
        ((self.processed_runs * 100) / self.total_runs).min(100) as u8
    }
}

pub type SyncProgressHandle = Arc<RwLock<SyncProgress>>;

/// Atomically checks-and-claims the "a sync is running" slot -- `true`
/// means the caller now owns the run and must call
/// `run_all_workflows_with_progress` (which itself sets `Completed`/
/// `Failed` on every exit path); `false` means one was already in
/// progress and the caller should do nothing. The check and the claim
/// happen under the same write lock, so two simultaneous callers (the
/// nightly timer's tick and a manual click landing at the same instant)
/// can't both observe `Idle` and both proceed.
pub fn try_claim_running(progress: &SyncProgressHandle) -> bool {
    let mut guard = progress.write();
    if guard.state == SyncState::Running {
        return false;
    }
    *guard = SyncProgress {
        state: SyncState::Running,
        ..Default::default()
    };
    true
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

type ExtractFn = fn(&[crate::process_street::FormField]) -> Vec<ExtractedPerson>;

const WORKFLOWS: &[(&str, &str, ExtractFn)] = &[
    (INTAKE_WORKFLOW_ID, "intake", extract_intake_people),
    (MERCHANT_ACCOUNT_WORKFLOW_ID, "merchant_account", extract_merchant_account_people),
    (CONTRACT_ORDER_WORKFLOW_ID, "contract_order", extract_contract_order_people),
];

/// Applies the delta check to exactly one run, refreshing it (deleting
/// and re-inserting its `ps_person_index` rows, then upserting
/// `ps_sync_state`) only when `needs_refresh` says so. Split out of
/// `sync_workflow_within` so `live_tests` can prove the skip behavior
/// against one specific known run without paying for a real `/form-fields`
/// fetch on every other real run in the workflow -- the expensive call
/// this whole module exists to avoid making unnecessarily.
///
/// Returns `(was_refreshed, people_indexed)`.
async fn sync_one_run(
    tx: &mut Transaction<'_, Postgres>,
    client: &ProcessStreetClient,
    workflow_key: &'static str,
    run: &crate::process_street::WorkflowRun,
    previously_synced_at: Option<DateTime<Utc>>,
    extract: ExtractFn,
) -> Result<(bool, usize), SyncError> {
    if !needs_refresh(previously_synced_at, run.updated_at()) {
        return Ok((false, 0));
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

    Ok((true, people.len()))
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

    for run in runs {
        let (refreshed, count) = sync_one_run(
            tx,
            client,
            workflow_key,
            run,
            existing.get(&run.id).copied(),
            extract,
        )
        .await?;
        if refreshed {
            runs_changed += 1;
            people_indexed += count;
        }
        on_processed();
    }

    Ok(SyncStats {
        workflow: workflow_key,
        runs_seen: runs.len(),
        runs_changed,
        people_indexed,
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
pub async fn run_all_workflows_with_progress(
    client: &ProcessStreetClient,
    db: &PgPool,
    progress: &SyncProgressHandle,
) {
    let mut per_workflow_runs = Vec::with_capacity(WORKFLOWS.len());
    for (workflow_id, workflow_key, extract) in WORKFLOWS {
        match client.list_workflow_runs(workflow_id).await {
            Ok(runs) => per_workflow_runs.push((*workflow_key, *extract, runs)),
            Err(err) => {
                fail(progress, err.to_string());
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
                fail(progress, err.to_string());
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
                fail(progress, err.to_string());
                return;
            }
        };

        if let Err(err) = tx.commit().await {
            fail(progress, err.to_string());
            return;
        }

        results.push(stats);
    }

    let mut guard = progress.write();
    guard.state = SyncState::Completed;
    guard.results = results;
}

fn fail(progress: &SyncProgressHandle, message: String) {
    let mut guard = progress.write();
    guard.state = SyncState::Failed;
    guard.error = Some(message);
}

/// Reads `client_ops.process_street_settings.sync_time` on the same
/// system role/RLS pattern as everything else in this module. Falls
/// back to `default_sync_time()` (never a panic, never blocking the
/// loop forever) on any read failure -- a transient DB hiccup should
/// delay this cycle's sync, not crash the background task.
async fn fetch_sync_time(db: &PgPool) -> NaiveTime {
    let result: Result<(NaiveTime,), sqlx::Error> = async {
        let mut tx = begin_rls_transaction(db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()]).await?;
        let row = sqlx::query_as("SELECT sync_time FROM client_ops.process_street_settings WHERE id = 1")
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(row)
    }
    .await;

    match result {
        Ok((sync_time,)) => sync_time,
        Err(err) => {
            tracing::error!(
                error = %err,
                "failed to read the configured Process Street sync time; defaulting to midnight UTC for this cycle"
            );
            default_sync_time()
        }
    }
}

/// The next UTC instant `sync_time` occurs at or after `now` -- today if
/// it hasn't passed yet, otherwise tomorrow. Pulled out as its own pure
/// function (no DB, no sleeping) so the date-rollover edge case has a
/// direct unit test, same reasoning `needs_refresh` above already uses.
fn next_occurrence(now: DateTime<Utc>, sync_time: NaiveTime) -> DateTime<Utc> {
    let today_at_sync_time = now.date_naive().and_time(sync_time).and_utc();
    if today_at_sync_time > now {
        today_at_sync_time
    } else {
        today_at_sync_time + ChronoDuration::days(1)
    }
}

/// Sleeps until the next occurrence (today if still ahead, otherwise
/// tomorrow) of the currently configured sync time -- re-read on every
/// call, not cached, so a settings change (`api::process_street_settings`)
/// takes effect on the very next cycle without needing a server
/// restart. Interpreted as UTC; see this module's own doc comment on
/// why no per-user timezone conversion happens here.
async fn sleep_until_next_scheduled_sync(db: &PgPool) {
    let sync_time = fetch_sync_time(db).await;

    let now = Utc::now();
    let next = next_occurrence(now, sync_time);

    let sleep_duration = (next - now)
        .to_std()
        .unwrap_or(std::time::Duration::from_secs(0));

    tracing::info!(
        next_sync_at = %next,
        "Process Street sync scheduled"
    );

    tokio::time::sleep(sleep_duration).await;
}

/// Spawns the scheduled sync loop -- runs only at the configured
/// `sync_time` (`client_ops.process_street_settings`, UTC, default
/// midnight) or when `api::clients_sync::start_sync` triggers one
/// manually. Deliberately does NOT also fire immediately on startup the
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

            run_all_workflows_with_progress(&client, &db, &progress).await;

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

    #[test]
    fn next_occurrence_is_later_today_when_the_sync_time_has_not_passed_yet() {
        let now = "2026-08-31T10:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let midnight = NaiveTime::from_hms_opt(0, 0, 0).unwrap();
        let noon = NaiveTime::from_hms_opt(12, 0, 0).unwrap();

        assert_eq!(
            next_occurrence(now, noon),
            "2026-08-31T12:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        // Midnight (00:00) has already passed today at 10:00 -- must
        // roll to tomorrow, not fire "in the past" or right now.
        assert_eq!(
            next_occurrence(now, midnight),
            "2026-09-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn next_occurrence_rolls_to_tomorrow_when_the_sync_time_has_already_passed_today() {
        let now = "2026-08-31T23:30:00Z".parse::<DateTime<Utc>>().unwrap();
        let ten_pm = NaiveTime::from_hms_opt(22, 0, 0).unwrap();

        assert_eq!(
            next_occurrence(now, ten_pm),
            "2026-09-01T22:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn next_occurrence_at_the_exact_current_instant_rolls_to_tomorrow_not_zero_sleep() {
        // An exact tie must not be treated as "still ahead" -- sleeping
        // for zero seconds and immediately re-triggering would turn one
        // scheduled sync into a tight loop right at the boundary.
        let now = "2026-08-31T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let midnight = NaiveTime::from_hms_opt(0, 0, 0).unwrap();

        assert_eq!(
            next_occurrence(now, midnight),
            "2026-09-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
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

        let (first_refreshed, first_count) =
            sync_one_run(&mut tx, &client, "intake", &run, None, extract_intake_people)
                .await
                .expect("first sync pass must succeed against the live API");

        assert!(first_refreshed, "a never-synced-before run must always refresh");
        assert!(first_count > 0, "at least one real Owner/DM/Manager person must have been indexed");

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
        let (second_refreshed, second_count) = sync_one_run(
            &mut tx,
            &client,
            "intake",
            &run,
            Some(run.updated_at()),
            extract_intake_people,
        )
        .await
        .expect("second sync pass must succeed");

        assert!(!second_refreshed, "an unchanged run must not need re-fetching");
        assert_eq!(second_count, 0);

        tx.rollback()
            .await
            .expect("rollback must succeed -- this is a one-time check, not a real sync");
    }
}
