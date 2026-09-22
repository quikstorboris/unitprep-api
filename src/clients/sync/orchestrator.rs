use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, Duration as ChronoDuration, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::auth::begin_rls_transaction;
use crate::client_ops::audit_log;
use crate::clients::fields::value_for_any;
use crate::clients::intake_mapping::map_intake_fields;
use crate::clients::known_workflows::{
    CONTRACT_ORDER_WORKFLOW_ID, INTAKE_WORKFLOW_ID, MERCHANT_ACCOUNT_WORKFLOW_ID,
};
use crate::clients::person_index::{
    extract_contract_order_people, extract_intake_people, extract_merchant_account_people,
    ExtractedPerson,
};
use crate::process_street::ProcessStreetClient;

use super::progress::{try_claim_running, SyncError, SyncProgressHandle, SyncState, SyncStats};
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
fn needs_refresh(
    previously_synced_at: Option<DateTime<Utc>>,
    current_updated_at: DateTime<Utc>,
) -> bool {
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
    (
        MERCHANT_ACCOUNT_WORKFLOW_ID,
        "merchant_account",
        extract_merchant_account_people,
    ),
    (
        CONTRACT_ORDER_WORKFLOW_ID,
        "contract_order",
        extract_contract_order_people,
    ),
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

    // A Merchant Account run's own `Business_DBA` -- falling back to
    // the two known key-drift variants PS's own template has used for
    // the same "internal name" concept (see
    // `merchant_account_correlation.rs`'s own doc comment on why this
    // is a more direct correlation signal than the run's title). Never
    // populated for intake/contract_order runs today, since neither
    // workflow's form has these fields -- `value_for_any` just returns
    // `None`, not an error, so this needs no `if workflow_key == ...`
    // branch.
    let business_dba = value_for_any(
        &fields,
        &[
            "Business_DBA".to_string(),
            "Facility_Name_in_CRM".to_string(),
            "Facility_Name_in_Zoho".to_string(),
        ],
    );

    sqlx::query(
        "INSERT INTO clients.ps_sync_state (workflow, ps_run_id, run_name, business_dba, ps_updated_at, last_synced_at)
         VALUES ($1, $2, $3, $4, $5, now())
         ON CONFLICT (workflow, ps_run_id) DO UPDATE SET
             run_name = EXCLUDED.run_name,
             business_dba = EXCLUDED.business_dba,
             ps_updated_at = EXCLUDED.ps_updated_at,
             last_synced_at = now()",
    )
    .bind(workflow_key)
    .bind(&run.id)
    .bind(&run.name)
    .bind(&business_dba)
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
///
/// `force`: when true, every run is treated as never-synced-before
/// (`previously_synced_at` is always `None`, regardless of what's
/// actually recorded), so `needs_refresh` unconditionally refetches
/// every single run in scope rather than skipping unchanged ones. Added
/// 2026-09-18 specifically so a newly-added locally-indexed field (like
/// `business_dba`) can be backfilled onto every already-indexed run --
/// the normal delta sync has no way to do that on its own, since a run
/// whose PS-side data hasn't changed since its last sync never gets
/// re-fetched otherwise, no matter how long ago that was or how much
/// this codebase now wants to extract from it. This is real cost, not
/// free: forcing every run treats the whole workflow as if none of it
/// had ever synced, which is exactly what a full backfill needs but
/// also exactly the number of Process Street requests the delta check
/// exists to avoid paying every single tick -- see this module's own
/// "2,500 requests/hour" gotcha before running this against a large
/// workflow.
async fn sync_runs_within(
    tx: &mut Transaction<'_, Postgres>,
    client: &ProcessStreetClient,
    workflow_key: &'static str,
    runs: &[crate::process_street::WorkflowRun],
    extract: ExtractFn,
    force: bool,
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
        let previously_synced_at = if force {
            None
        } else {
            existing.get(&run.id).copied()
        };
        let outcome = sync_one_run(
            tx,
            client,
            workflow_key,
            run,
            previously_synced_at,
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
///
/// `force`: see `sync_runs_within`'s own doc comment -- passed straight
/// through unchanged, applies to every workflow in this one pass.
pub async fn run_all_workflows_with_progress(
    client: &ProcessStreetClient,
    db: &PgPool,
    progress: &SyncProgressHandle,
    actor_user_id: Uuid,
    force: bool,
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

    let total_runs: usize = per_workflow_runs
        .iter()
        .map(|(_, _, runs)| runs.len())
        .sum();
    progress.write().total_runs = total_runs;

    let mut results = Vec::with_capacity(per_workflow_runs.len());

    for (workflow_key, extract, runs) in &per_workflow_runs {
        let mut tx =
            match begin_rls_transaction(db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()]).await {
                Ok(tx) => tx,
                Err(err) => {
                    fail(db, progress, actor_user_id, err.to_string()).await;
                    return;
                }
            };

        let stats_result = sync_runs_within(
            &mut tx,
            client,
            workflow_key,
            runs,
            *extract,
            force,
            || {
                progress.write().processed_runs += 1;
            },
        )
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
            "force": force,
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

#[derive(Debug, Clone)]
struct ScheduleConfig {
    /// `"interval"` | `"daily_time"`.
    mode: String,
    interval_hours: i16,
    sync_time: Option<NaiveTime>,
    sync_timezone: Option<String>,
}

fn default_schedule_config() -> ScheduleConfig {
    ScheduleConfig {
        mode: "interval".to_string(),
        interval_hours: default_sync_interval_hours(),
        sync_time: None,
        sync_timezone: None,
    }
}

/// Reads the whole schedule config off `client_ops.process_street_settings`
/// on the same system role/RLS pattern as everything else in this
/// module. Falls back to `default_schedule_config()` (never a panic,
/// never blocking the loop forever) on any read failure -- a transient
/// DB hiccup should delay this cycle's sync, not crash the background
/// task.
async fn fetch_schedule_config(db: &PgPool) -> ScheduleConfig {
    let result: Result<(String, i16, Option<NaiveTime>, Option<String>), sqlx::Error> = async {
        let mut tx = begin_rls_transaction(db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()]).await?;
        let row = sqlx::query_as(
            "SELECT schedule_mode, sync_interval_hours, sync_time, sync_timezone
               FROM client_ops.process_street_settings WHERE id = 1",
        )
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }
    .await;

    match result {
        Ok((mode, interval_hours, sync_time, sync_timezone)) => ScheduleConfig {
            mode,
            interval_hours,
            sync_time,
            sync_timezone,
        },
        Err(err) => {
            tracing::error!(
                error = %err,
                "failed to read the configured Process Street sync schedule; defaulting to a 24h interval for this cycle"
            );
            default_schedule_config()
        }
    }
}

/// The next UTC instant `sync_time` occurs at or after `now`, in `tz` --
/// today if it hasn't passed yet there, otherwise tomorrow. Pulled out
/// as its own pure function (no DB, no sleeping) so the DST/rollover
/// edge cases have direct unit tests, same reasoning `needs_refresh`
/// above already uses.
///
/// A DST transition can make a given local wall-clock instant either
/// ambiguous (repeated, "fall back") or nonexistent (skipped, "spring
/// forward"). Ambiguous resolves to the earliest of the two real
/// instants; nonexistent nudges the local time forward by an hour (a
/// DST gap is always under two hours) and resolves that instead. Both
/// only matter on the one or two days a year the configured `sync_time`
/// happens to fall exactly inside that zone's transition window -- a
/// tick landing an hour early/late that day is an acceptable trade for
/// never blocking the loop entirely.
fn next_daily_occurrence(now: DateTime<Utc>, sync_time: NaiveTime, tz: Tz) -> DateTime<Utc> {
    fn resolve(tz: Tz, naive: chrono::NaiveDateTime, fallback: DateTime<Utc>) -> DateTime<Utc> {
        match tz.from_local_datetime(&naive) {
            chrono::LocalResult::Single(dt) => dt.with_timezone(&Utc),
            chrono::LocalResult::Ambiguous(earliest, _latest) => earliest.with_timezone(&Utc),
            chrono::LocalResult::None => tz
                .from_local_datetime(&(naive + ChronoDuration::hours(1)))
                .single()
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or(fallback),
        }
    }

    let today_naive = now.with_timezone(&tz).date_naive().and_time(sync_time);
    let today_at_time = resolve(tz, today_naive, now);

    if today_at_time > now {
        today_at_time
    } else {
        resolve(tz, today_naive + ChronoDuration::days(1), now)
    }
}

/// Sleeps until the next scheduled sync per the currently configured
/// mode -- re-read on every call, not cached, so a settings change
/// (`api::process_street_settings`) takes effect on the very next cycle
/// without needing a server restart. `"interval"` sleeps for the
/// configured number of hours (every tick is simply "interval hours
/// after the last one finished" -- see `clients::sync`'s own module doc
/// for why a much shorter interval than the old once-daily default is
/// realistic at all). `"daily_time"` sleeps until the next occurrence of
/// the configured clock time in the configured timezone -- falls back
/// to the default interval (logged as an error, not a panic) if the
/// stored timezone somehow isn't parseable, which should never happen
/// given `api::process_street_settings` only ever writes a value from
/// its own closed, validated list.
async fn sleep_until_next_scheduled_sync(db: &PgPool) {
    let config = fetch_schedule_config(db).await;

    let sleep_duration = if config.mode == "daily_time" {
        let tz = config
            .sync_timezone
            .as_deref()
            .and_then(|zone| Tz::from_str(zone).ok());

        match (config.sync_time, tz) {
            (Some(sync_time), Some(tz)) => {
                let now = Utc::now();
                let next = next_daily_occurrence(now, sync_time, tz);
                tracing::info!(
                    next_sync_at = %next,
                    timezone = %tz,
                    "Process Street sync scheduled (daily_time)"
                );
                (next - now)
                    .to_std()
                    .unwrap_or(std::time::Duration::from_secs(0))
            }
            _ => {
                tracing::error!(
                    sync_timezone = ?config.sync_timezone,
                    "schedule_mode is daily_time but sync_time/sync_timezone is missing or unparseable; falling back to the default interval for this cycle"
                );
                std::time::Duration::from_secs((default_sync_interval_hours().max(1) as u64) * 3600)
            }
        }
    } else {
        let interval_hours = config.interval_hours;
        tracing::info!(
            next_sync_at = %(Utc::now() + ChronoDuration::hours(i64::from(interval_hours))),
            interval_hours,
            "Process Street sync scheduled (interval)"
        );
        std::time::Duration::from_secs((interval_hours.max(1) as u64) * 3600)
    };

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

            // Never forced -- the nightly timer is the routine delta
            // pass this whole module exists to make cheap. Forcing is
            // an explicit, occasional choice made through the manual
            // "Sync Now" trigger, never automatic.
            run_all_workflows_with_progress(&client, &db, &progress, SYSTEM_USER_ID, false).await;

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
    fn next_daily_occurrence_is_later_today_in_utc_when_the_time_has_not_passed_yet() {
        let now = "2026-08-31T10:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let noon = NaiveTime::from_hms_opt(12, 0, 0).unwrap();

        assert_eq!(
            next_daily_occurrence(now, noon, Tz::UTC),
            "2026-08-31T12:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn next_daily_occurrence_rolls_to_tomorrow_when_the_time_has_already_passed_today() {
        let now = "2026-08-31T23:30:00Z".parse::<DateTime<Utc>>().unwrap();
        let ten_pm = NaiveTime::from_hms_opt(22, 0, 0).unwrap();

        assert_eq!(
            next_daily_occurrence(now, ten_pm, Tz::UTC),
            "2026-09-01T22:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn next_daily_occurrence_at_the_exact_current_instant_rolls_to_tomorrow_not_zero_sleep() {
        // An exact tie must not be treated as "still ahead" -- sleeping
        // for zero seconds and immediately re-triggering would turn one
        // scheduled sync into a tight loop right at the boundary.
        let now = "2026-08-31T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let midnight = NaiveTime::from_hms_opt(0, 0, 0).unwrap();

        assert_eq!(
            next_daily_occurrence(now, midnight, Tz::UTC),
            "2026-09-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn next_daily_occurrence_converts_a_real_timezone_to_the_correct_utc_instant() {
        // 3:00 AM Pacific in late August is PDT (UTC-7) -- 10:00 UTC.
        let now = "2026-08-31T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let three_am = NaiveTime::from_hms_opt(3, 0, 0).unwrap();

        assert_eq!(
            next_daily_occurrence(now, three_am, chrono_tz::America::Los_Angeles),
            "2026-08-31T10:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn next_daily_occurrence_does_not_panic_across_a_real_spring_forward_gap() {
        // US DST began 2026-03-08 at 02:00 Pacific (clocks jump straight
        // to 03:00) -- 02:30 that day never happened locally. Only
        // asserts this resolves to *something* sane (a real instant,
        // not a panic/unwrap failure) -- the exact chosen instant during
        // a gap is a documented, acceptable imprecision, not a contract.
        let now = "2026-03-08T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let two_thirty_am = NaiveTime::from_hms_opt(2, 30, 0).unwrap();

        let next = next_daily_occurrence(now, two_thirty_am, chrono_tz::America::Los_Angeles);
        assert!(next > now);
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

        let ps_config = ProcessStreetConfig::from_env()
            .expect("PROCESS_STREET_API_KEY must be set in .env.local");
        let client = ProcessStreetClient::new(ps_config);

        let matches = client
            .search_workflow_runs_by_name(INTAKE_WORKFLOW_ID, "highway")
            .await
            .expect("search must succeed against the live API");
        let run = matches
            .into_iter()
            .find(|r| r.name == "Highway 20 Self Storage - QMS Onboarding")
            .expect("Highway 20's Intake run must be found");

        let db =
            crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
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

        let ps_config = ProcessStreetConfig::from_env()
            .expect("PROCESS_STREET_API_KEY must be set in .env.local");
        let client = ProcessStreetClient::new(ps_config);

        let matches = client
            .search_workflow_runs_by_name(MERCHANT_ACCOUNT_WORKFLOW_ID, "highway 20")
            .await
            .expect("search must succeed against the live API");
        let run = matches
            .into_iter()
            .find(|r| r.name == "Prairie Enterprises (Highway 20)")
            .expect("Highway 20's own Merchant Account run must be found");

        let db =
            crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
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

        let ps_config = ProcessStreetConfig::from_env()
            .expect("PROCESS_STREET_API_KEY must be set in .env.local");
        let client = ProcessStreetClient::new(ps_config);

        let db =
            crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
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

        tx.rollback()
            .await
            .expect("rollback must succeed -- this is a one-time check, must not persist against the real row");
    }
}
