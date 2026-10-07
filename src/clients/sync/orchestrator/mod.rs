#[cfg(test)]
mod batch_tests;
#[cfg(test)]
mod live_tests;
mod schedule;
#[cfg(test)]
mod tests;

pub use schedule::start_background_sync_task;

use super::progress::{SyncError, SyncProgressHandle, SyncState, SyncStats};
use super::refresh::{refresh_matching_company, refresh_matching_facility};
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
use crate::integrations::http::join_all_bounded;
use crate::process_street::{FormField, ProcessStreetClient, ProcessStreetError, WorkflowRun};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use uuid::Uuid;

/// See this module's own doc comment for why a fixed, non-empty
/// placeholder is correct here, and why it also covers the write
/// policies `client_ops::vendor_format`'s read-only use of the same
/// pattern never had to.
pub(super) const SYSTEM_USER_ID: Uuid = Uuid::nil();
pub(super) const SYSTEM_ROLE: &str = "onboarding_manager";

/// Fallback only -- used when `integrations.process_street_settings`
/// can't be read at all (a transient DB error), never as the normal
/// path. The settings row itself defaults to the same value.
pub(super) fn default_sync_interval_hours() -> i16 {
    24
}

/// Pure decision at the heart of the delta check -- pulled out of the
/// DB/network-heavy loop below so it has its own direct unit tests, no
/// fixture or live call needed. `None` (never synced before) always
/// needs a refresh; otherwise a run only needs one when PS's own
/// `updatedDate` has moved past what was last recorded.
pub(super) fn needs_refresh(
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
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct RunSyncOutcome {
    pub(super) person_index_refreshed: bool,
    pub(super) people_indexed: usize,
    pub(super) company_refreshed: bool,
    pub(super) facility_refreshed: bool,
}

pub(super) type ExtractFn = fn(&[crate::process_street::FormField]) -> Vec<ExtractedPerson>;

pub(super) const WORKFLOWS: &[(&str, &str, ExtractFn)] = &[
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

/// How many runs are fetched together (concurrently, bounded) and then
/// written in ONE short transaction. Small enough that a failure mid-sync
/// loses little (everything already committed stays committed and is
/// skipped by the next delta check), large enough that the per-batch
/// statements (one delete, one insert, one upsert) amortize their round
/// trips over many runs.
pub(super) const RUN_BATCH_SIZE: usize = 25;

/// A run that needs refreshing, with the form fields just fetched for it.
pub(super) struct FetchedRun<'a> {
    pub(super) run: &'a WorkflowRun,
    pub(super) fields: Vec<FormField>,
}

/// What one batch write did -- the building block of `SyncStats`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct BatchOutcome {
    pub(super) runs_changed: usize,
    pub(super) people_indexed: usize,
    pub(super) companies_refreshed: usize,
    pub(super) facilities_refreshed: usize,
}

/// The pure half of the delta check: which of `runs` actually need a
/// `form-fields` fetch. A run is skipped when `ps_sync_state` already holds
/// an `updatedDate` at least as new as PS's (see `needs_refresh`); `force`
/// treats every run as never-synced. A run id that appears twice in the
/// list (PS's pagination can repeat one if the list changes while it is
/// being walked) is refreshed once -- two rows with the same key in one
/// batched upsert would otherwise be an error.
pub(super) fn runs_needing_refresh<'a>(
    runs: &'a [WorkflowRun],
    existing: &HashMap<String, DateTime<Utc>>,
    force: bool,
) -> Vec<&'a WorkflowRun> {
    let mut seen: HashSet<&str> = HashSet::new();

    runs.iter()
        .filter(|run| {
            let previously_synced_at = if force {
                None
            } else {
                existing.get(&run.id).copied()
            };
            needs_refresh(previously_synced_at, run.updated_at()) && seen.insert(run.id.as_str())
        })
        .collect()
}

/// The database-only half: writes one batch of fetched runs inside the
/// caller's transaction -- a single `DELETE` of every run's old
/// `ps_person_index` rows, a single multi-row `INSERT` of the fresh people,
/// a single multi-row upsert of `ps_sync_state`, and -- for an Intake run
/// only -- refreshing any already-imported Company/Facility whose own
/// `ps_intake_run_id` matches (`refresh_matching_company` /
/// `refresh_matching_facility`, still one pair per run).
///
/// This used to be `sync_one_run`: per run, a fetch from Process Street
/// INSIDE the open transaction, a `DELETE`, one `INSERT` per person, and an
/// upsert -- so a workflow of thousands of changed runs held one pooled
/// connection in one transaction for minutes while it waited on the network
/// and issued tens of thousands of single-row statements. Keeping the
/// network OUT of this function is what lets the transaction stay short.
///
/// Company/Facility refresh is Intake-only for now: a company's fields
/// are seeded from whichever facility's own Intake run answered "first
/// time = Yes" (see `clients.companies.ps_intake_run_id`'s own migration
/// comment), not from a persisted link to a Merchant Account run -- there
/// is no such link stored today, so a later change to that Merchant
/// Account run's own data has nothing to refresh against yet.
pub(super) async fn apply_fetched_runs(
    tx: &mut Transaction<'_, Postgres>,
    workflow_key: &'static str,
    extract: ExtractFn,
    fetched: &[FetchedRun<'_>],
) -> Result<BatchOutcome, SyncError> {
    if fetched.is_empty() {
        return Ok(BatchOutcome::default());
    }

    let run_ids: Vec<&str> = fetched.iter().map(|f| f.run.id.as_str()).collect();
    sqlx::query("DELETE FROM clients.ps_person_index WHERE workflow = $1 AND ps_run_id = ANY($2)")
        .bind(workflow_key)
        .bind(&run_ids)
        .execute(&mut **tx)
        .await?;

    // A Merchant Account run's own `Business_DBA` -- falling back to
    // the two known key-drift variants PS's own template has used for
    // the same "internal name" concept (see
    // `merchant_account_correlation.rs`'s own doc comment on why this
    // is a more direct correlation signal than the run's title). Never
    // populated for intake/contract_order runs today, since neither
    // workflow's form has these fields -- `value_for_any` just returns
    // `None`, not an error, so this needs no `if workflow_key == ...`
    // branch.
    let business_dba_keys = [
        "Business_DBA".to_string(),
        "Facility_Name_in_CRM".to_string(),
        "Facility_Name_in_Zoho".to_string(),
    ];

    let mut person_run_ids: Vec<&str> = Vec::new();
    let mut person_run_names: Vec<&str> = Vec::new();
    let mut person_full_names: Vec<String> = Vec::new();
    let mut person_emails: Vec<Option<String>> = Vec::new();
    let mut person_phones: Vec<Option<String>> = Vec::new();
    let mut person_roles: Vec<&'static str> = Vec::new();

    let mut state_run_names: Vec<&str> = Vec::new();
    let mut state_business_dbas: Vec<Option<String>> = Vec::new();
    let mut state_updated_ats: Vec<DateTime<Utc>> = Vec::new();

    for fetched_run in fetched {
        let run = fetched_run.run;

        for person in extract(&fetched_run.fields) {
            person_run_ids.push(&run.id);
            person_run_names.push(&run.name);
            person_full_names.push(person.full_name);
            person_emails.push(person.email);
            person_phones.push(person.phone);
            person_roles.push(person.role);
        }

        state_run_names.push(&run.name);
        state_business_dbas.push(value_for_any(&fetched_run.fields, &business_dba_keys));
        state_updated_ats.push(run.updated_at());
    }

    let people_indexed = person_run_ids.len();
    if people_indexed > 0 {
        sqlx::query(
            "INSERT INTO clients.ps_person_index
                 (workflow, ps_run_id, run_name, full_name, email, phone, role)
             SELECT $1, run_id, run_name, full_name, email, phone, role
               FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::text[])
                    AS t(run_id, run_name, full_name, email, phone, role)",
        )
        .bind(workflow_key)
        .bind(&person_run_ids)
        .bind(&person_run_names)
        .bind(&person_full_names)
        .bind(&person_emails)
        .bind(&person_phones)
        .bind(&person_roles)
        .execute(&mut **tx)
        .await?;
    }

    sqlx::query(
        "INSERT INTO clients.ps_sync_state (workflow, ps_run_id, run_name, business_dba, ps_updated_at, last_synced_at)
         SELECT $1, run_id, run_name, business_dba, ps_updated_at, now()
           FROM UNNEST($2::text[], $3::text[], $4::text[], $5::timestamptz[])
                AS t(run_id, run_name, business_dba, ps_updated_at)
         ON CONFLICT (workflow, ps_run_id) DO UPDATE SET
             run_name = EXCLUDED.run_name,
             business_dba = EXCLUDED.business_dba,
             ps_updated_at = EXCLUDED.ps_updated_at,
             last_synced_at = now()",
    )
    .bind(workflow_key)
    .bind(&run_ids)
    .bind(&state_run_names)
    .bind(&state_business_dbas)
    .bind(&state_updated_ats)
    .execute(&mut **tx)
    .await?;

    let mut companies_refreshed = 0;
    let mut facilities_refreshed = 0;
    if workflow_key == "intake" {
        for fetched_run in fetched {
            let mapped = map_intake_fields(&fetched_run.fields);
            if refresh_matching_company(tx, &fetched_run.run.id, &mapped.company).await? {
                companies_refreshed += 1;
            }
            if refresh_matching_facility(tx, &fetched_run.run.id, &mapped.facility).await? {
                facilities_refreshed += 1;
            }
        }
    }

    Ok(BatchOutcome {
        runs_changed: fetched.len(),
        people_indexed,
        companies_refreshed,
        facilities_refreshed,
    })
}

/// Syncs a pre-fetched list of one workflow's runs into
/// `ps_sync_state`/`ps_person_index`.
///
/// Three phases per workflow, in this order, so that **no database
/// transaction is ever open while Process Street is being called**:
///
/// 1. a short read transaction for the already-recorded `updatedDate`s;
/// 2. for each batch of `batch_size` runs that need a refresh, fetch their
///    form fields CONCURRENTLY (bounded -- see `join_all_bounded`) with
///    nothing held;
/// 3. write that batch in its own short transaction (`apply_fetched_runs`)
///    and commit it before the next batch's fetch starts.
///
/// Commits are per batch, not per workflow: a failure partway (Process
/// Street erroring after its retries, or a database error) stops the sync
/// -- same fail-fast as before, since a partial percentage that silently
/// stalls is worse than a clearly-`Failed` state -- but everything already
/// committed stays committed, and the next delta check skips those runs
/// (their `ps_sync_state` rows were written in the same transaction as
/// their person-index rows, so a run is never half-recorded). Previously a
/// failure rolled back the whole workflow, so the next attempt redid all of
/// it.
///
/// `fetch` gets a run id and returns that run's form fields: the real sync
/// passes `ProcessStreetClient::get_run_form_fields`; tests pass a fake, so
/// the whole pipeline is provable without Process Street.
///
/// `on_processed` fires once per run -- immediately for each run the delta
/// check skips, and after each batch commits for the rest --
/// `run_all_workflows_with_progress` uses it to advance a shared progress
/// counter.
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
#[allow(clippy::too_many_arguments)]
pub(super) async fn sync_workflow_runs<F, Fut>(
    db: &PgPool,
    fetch: F,
    workflow_key: &'static str,
    runs: &[WorkflowRun],
    extract: ExtractFn,
    force: bool,
    batch_size: usize,
    mut on_processed: impl FnMut(),
) -> Result<SyncStats, SyncError>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Vec<FormField>, ProcessStreetError>>,
{
    let system_roles = [SYSTEM_ROLE.to_string()];

    let existing: HashMap<String, DateTime<Utc>> = {
        let mut tx = begin_rls_transaction(db, SYSTEM_USER_ID, &system_roles).await?;
        let rows = sqlx::query_as(
            "SELECT ps_run_id, ps_updated_at FROM clients.ps_sync_state WHERE workflow = $1",
        )
        .bind(workflow_key)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        rows.into_iter().collect()
    };

    let to_refresh = runs_needing_refresh(runs, &existing, force);

    // Every run the delta check skips is "processed" right away.
    for _ in 0..(runs.len() - to_refresh.len()) {
        on_processed();
    }

    let mut stats = SyncStats {
        workflow: workflow_key,
        runs_seen: runs.len(),
        runs_changed: 0,
        people_indexed: 0,
        companies_refreshed: 0,
        facilities_refreshed: 0,
    };

    for batch in to_refresh.chunks(batch_size.max(1)) {
        // Network phase: nothing held while Process Street answers.
        let results = join_all_bounded(batch.iter().map(|run| fetch(run.id.clone()))).await;

        let mut fetched = Vec::with_capacity(batch.len());
        for (run, result) in batch.iter().zip(results) {
            fetched.push(FetchedRun {
                run,
                fields: result?,
            });
        }

        // Write phase: one short transaction for the whole batch.
        let mut tx = begin_rls_transaction(db, SYSTEM_USER_ID, &system_roles).await?;
        let outcome = match apply_fetched_runs(&mut tx, workflow_key, extract, &fetched).await {
            Ok(outcome) => outcome,
            Err(err) => {
                let _ = tx.rollback().await;
                return Err(err);
            }
        };
        tx.commit().await?;

        stats.runs_changed += outcome.runs_changed;
        stats.people_indexed += outcome.people_indexed;
        stats.companies_refreshed += outcome.companies_refreshed;
        stats.facilities_refreshed += outcome.facilities_refreshed;

        for _ in batch {
            on_processed();
        }
    }

    Ok(stats)
}

/// One run, fetched and written inside the CALLER's transaction -- kept
/// (test-only) so the `#[ignore]`d live tests below can still prove the
/// pipeline against the real API and then roll back ("prove it, then roll
/// back"), which the production path, committing per batch, cannot do.
#[cfg(test)]
pub(super) async fn sync_one_run(
    tx: &mut Transaction<'_, Postgres>,
    client: &ProcessStreetClient,
    workflow_key: &'static str,
    run: &WorkflowRun,
    previously_synced_at: Option<DateTime<Utc>>,
    extract: ExtractFn,
) -> Result<RunSyncOutcome, SyncError> {
    if !needs_refresh(previously_synced_at, run.updated_at()) {
        return Ok(RunSyncOutcome::default());
    }

    let fields = client.get_run_form_fields(&run.id).await?;
    let outcome =
        apply_fetched_runs(tx, workflow_key, extract, &[FetchedRun { run, fields }]).await?;

    Ok(RunSyncOutcome {
        person_index_refreshed: true,
        people_indexed: outcome.people_indexed,
        company_refreshed: outcome.companies_refreshed > 0,
        facility_refreshed: outcome.facilities_refreshed > 0,
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
    // The three workflows' (cheap) run lists are independent, so they are
    // listed together rather than one after another.
    let listings = futures::future::try_join_all(
        WORKFLOWS
            .iter()
            .map(|(workflow_id, _, _)| client.list_workflow_runs(workflow_id)),
    )
    .await;

    let per_workflow_runs: Vec<_> = match listings {
        Ok(lists) => WORKFLOWS
            .iter()
            .zip(lists)
            .map(|((_, workflow_key, extract), runs)| (*workflow_key, *extract, runs))
            .collect(),
        Err(err) => {
            fail(db, progress, actor_user_id, err.to_string()).await;
            return;
        }
    };

    let total_runs: usize = per_workflow_runs
        .iter()
        .map(|(_, _, runs)| runs.len())
        .sum();
    progress.write().total_runs = total_runs;

    let mut results = Vec::with_capacity(per_workflow_runs.len());

    for (workflow_key, extract, runs) in &per_workflow_runs {
        let stats_result = sync_workflow_runs(
            db,
            |run_id: String| async move { client.get_run_form_fields(&run_id).await },
            workflow_key,
            runs,
            *extract,
            force,
            RUN_BATCH_SIZE,
            || {
                progress.write().processed_runs += 1;
            },
        )
        .await;

        match stats_result {
            Ok(stats) => results.push(stats),
            Err(err) => {
                fail(db, progress, actor_user_id, err.to_string()).await;
                return;
            }
        }
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
pub(super) async fn fail(
    db: &PgPool,
    progress: &SyncProgressHandle,
    actor_user_id: Uuid,
    message: String,
) {
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
