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
//! this module's real caller -- `clients::ingest` calls
//! `get_run_form_fields`/`get_run_tasks` for both the Intake/Progress
//! and New Merchant Account workflows. `list_workflows`/
//! `list_workflow_runs` have no caller yet (they're for the future
//! search/discovery flow, Phase 2+), so those two specifically stay
//! legitimately unused for now -- see their own `#[allow(dead_code)]`.

mod client;
mod config;

pub use client::{FormField, ProcessStreetClient, ProcessStreetError, Task};
// No caller yet -- see the doc comments on `Workflow`/`ProcessStreetClient::new`
// in client.rs and on `ProcessStreetConfig::from_env` in config.rs.
#[allow(unused_imports)]
pub use client::{Workflow, WorkflowRun};
#[allow(unused_imports)]
pub use config::ProcessStreetConfig;
