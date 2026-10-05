//! Read-only Process Street (PS) access -- the source for OO's client
//! records (see the vault: work/active/UnitPrep/Process Street
//! Integration/). Three workflows matter: Intake/Progress, New Merchant
//! Account, Contract Order.
//!
//! **Read-only is a hard constraint, not just today's scope.** PS is a
//! live ops system the onboarding team depends on daily; nothing here
//! may call a write endpoint without that being explicitly revisited.
//! `ProcessStreetClient` has no `create`/`update`/`delete` method for
//! exactly this reason.
//!
//! The `clients` module (mapping/ingestion layer, `src/clients/`) is
//! this module's real caller: `clients::create` and the Re-sync/Elavon
//! handlers call `get_run_form_fields`/`get_run_tasks`, and `clients::search`/
//! `clients::sync` call `list_workflow_runs`/`search_workflow_runs_by_name`
//! for the two search paths `api::clients_search` exposes.
//! (Listing workflow *templates*, distinct from workflow *runs*, was never
//! needed -- every workflow id is a known constant -- and its unused method
//! was removed.)

mod client;
mod config;

pub use client::{FormField, ProcessStreetClient, ProcessStreetError, Task, WorkflowRun};
pub use config::ProcessStreetConfig;
