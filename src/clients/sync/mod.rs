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
//!
//! 2026-09-09: split into three submodules -- `progress` (the pure
//! pollable state `SyncProgress`/`SyncState`/`try_claim_running`),
//! `refresh` (diffing/merging a fresh Process Street pull onto an
//! existing `clients.companies`/`clients.facilities` row, respecting
//! `manually_edited_fields` -- also `api::clients_resync`'s own
//! dependency), and `orchestrator` (the actual sync loop: one run, one
//! workflow, all workflows with progress, the background timer). Each
//! was previously stacked in one ~1400 line file; only these re-exports
//! below are used outside this module, so every external `clients::sync::*`
//! path is unchanged.

mod orchestrator;
mod progress;
mod refresh;

pub use orchestrator::{run_all_workflows_with_progress, start_background_sync_task};
pub use progress::{try_claim_running, SyncProgress, SyncProgressHandle, SyncState};
pub(crate) use refresh::{
    apply_company_refresh, apply_facility_refresh, company_field_value, facility_field_value,
    facility_fields_that_differ,
};
