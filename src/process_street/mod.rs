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
//! this module's real caller: `clients::ingest` calls
//! `get_run_form_fields`/`get_run_tasks`, and `clients::search`/
//! `clients::sync` call `list_workflow_runs`/`search_workflow_runs_by_name`
//! for the two search paths `api::clients_search` exposes.
//! `list_workflows`/`Workflow` (listing workflow *templates*, distinct
//! from workflow *runs*) still have no caller -- see their own
//! `#[allow(dead_code)]` in client.rs.

mod client;
mod config;

pub use client::{FormField, ProcessStreetClient, ProcessStreetError, Task, WorkflowRun};
// No caller yet -- see the doc comment on `Workflow` in client.rs.
#[allow(unused_imports)]
pub use client::Workflow;
pub use config::ProcessStreetConfig;
