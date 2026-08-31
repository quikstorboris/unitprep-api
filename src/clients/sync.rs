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
use std::time::Duration;

use chrono::{DateTime, Utc};
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

/// Nightly, per Boris's own stated preference -- not yet configurable
/// (that's the future Integrations settings page's job); a fixed
/// constant is the honest Phase-2-scope version of that.
const SYNC_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

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

/// Syncs one workflow's runs into `ps_sync_state`/`ps_person_index`
/// within an already-open transaction -- the caller decides whether to
/// commit or roll back, same discipline `clients::ingest::ingest_facility`
/// and every `clients::repository` function already use.
async fn sync_workflow_within(
    tx: &mut Transaction<'_, Postgres>,
    client: &ProcessStreetClient,
    workflow_id: &str,
    workflow_key: &'static str,
    extract: ExtractFn,
) -> Result<SyncStats, SyncError> {
    let runs = client.list_workflow_runs(workflow_id).await?;

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

    for run in &runs {
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
    }

    Ok(SyncStats {
        workflow: workflow_key,
        runs_seen: runs.len(),
        runs_changed,
        people_indexed,
    })
}

/// Opens its own RLS transaction and commits -- the entry point every
/// real caller (the background task, a future manual "Sync Now" button)
/// uses. Each of the three workflows gets its own transaction, so one
/// workflow's failure (a transient PS error, say) doesn't roll back
/// progress already made on the others.
pub async fn sync_workflow(
    client: &ProcessStreetClient,
    db: &PgPool,
    workflow_id: &str,
    workflow_key: &'static str,
    extract: ExtractFn,
) -> Result<SyncStats, SyncError> {
    let mut tx = begin_rls_transaction(db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()]).await?;
    let stats = sync_workflow_within(&mut tx, client, workflow_id, workflow_key, extract).await?;
    tx.commit().await?;
    Ok(stats)
}

/// Syncs all three workflows. Returns one result per workflow rather
/// than short-circuiting on the first error, so a Contract Order outage
/// (say) doesn't also block Intake/Merchant Account from indexing.
pub async fn sync_all_workflows(client: &ProcessStreetClient, db: &PgPool) -> Vec<Result<SyncStats, SyncError>> {
    let mut results = Vec::with_capacity(WORKFLOWS.len());
    for (workflow_id, workflow_key, extract) in WORKFLOWS {
        results.push(sync_workflow(client, db, workflow_id, workflow_key, *extract).await);
    }
    results
}

/// Spawns the nightly sync loop -- same shape as
/// `client_ops::vendor_format::start_refresh_task`: spawned once at
/// startup, loops forever, one failed tick is logged and skipped rather
/// than ending the task. Unlike that task, `tokio::time::interval`'s
/// first tick fires immediately, so the first real sync happens right
/// at startup, not a full day later.
pub fn start_background_sync_task(client: std::sync::Arc<ProcessStreetClient>, db: PgPool) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SYNC_INTERVAL);

        loop {
            interval.tick().await;

            for result in sync_all_workflows(&client, &db).await {
                match result {
                    Ok(stats) => tracing::info!(
                        workflow = stats.workflow,
                        runs_seen = stats.runs_seen,
                        runs_changed = stats.runs_changed,
                        people_indexed = stats.people_indexed,
                        "Process Street person-index sync completed"
                    ),
                    Err(err) => tracing::error!(
                        error = %err,
                        "Process Street person-index sync failed for one workflow; other workflows still ran"
                    ),
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;

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
