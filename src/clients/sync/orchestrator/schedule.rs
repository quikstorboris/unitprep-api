//! When the background sync runs: the saved schedule, the next daily occurrence in the configured time zone, sleeping until it, and the task that ties it together.

use super::super::progress::{try_claim_running, SyncProgressHandle, SyncState};
use super::{
    default_sync_interval_hours, run_all_workflows_with_progress, SYSTEM_ROLE, SYSTEM_USER_ID,
};
use crate::auth::begin_rls_transaction;
use crate::process_street::ProcessStreetClient;
use chrono::TimeZone;
use chrono::{DateTime, Duration as ChronoDuration, NaiveTime, Utc};
use chrono_tz::Tz;
use sqlx::PgPool;
use std::str::FromStr;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(super) struct ScheduleConfig {
    /// `"interval"` | `"daily_time"`.
    pub(super) mode: String,
    pub(super) interval_hours: i16,
    pub(super) sync_time: Option<NaiveTime>,
    pub(super) sync_timezone: Option<String>,
}

pub(super) fn default_schedule_config() -> ScheduleConfig {
    ScheduleConfig {
        mode: "interval".to_string(),
        interval_hours: default_sync_interval_hours(),
        sync_time: None,
        sync_timezone: None,
    }
}

/// Reads the whole schedule config off `integrations.process_street_settings`
/// on the same system role/RLS pattern as everything else in this
/// module. Falls back to `default_schedule_config()` (never a panic,
/// never blocking the loop forever) on any read failure -- a transient
/// DB hiccup should delay this cycle's sync, not crash the background
/// task.
pub(super) async fn fetch_schedule_config(db: &PgPool) -> ScheduleConfig {
    let result: Result<(String, i16, Option<NaiveTime>, Option<String>), sqlx::Error> = async {
        let mut tx = begin_rls_transaction(db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()]).await?;
        let row = sqlx::query_as(
            "SELECT schedule_mode, sync_interval_hours, sync_time, sync_timezone
               FROM integrations.process_street_settings WHERE id = 1",
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
pub(super) fn next_daily_occurrence(
    now: DateTime<Utc>,
    sync_time: NaiveTime,
    tz: Tz,
) -> DateTime<Utc> {
    pub(super) fn resolve(
        tz: Tz,
        naive: chrono::NaiveDateTime,
        fallback: DateTime<Utc>,
    ) -> DateTime<Utc> {
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
pub(super) async fn sleep_until_next_scheduled_sync(db: &PgPool) {
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
/// `sync_interval_hours` (`integrations.process_street_settings`, default
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
