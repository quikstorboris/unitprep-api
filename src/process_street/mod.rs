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
//! This module is Phase 0 only (schema-matching migration:
//! `20260828120000_create_process_street_client_tables`) -- the actual
//! PS-run -> `client_ops.*` mapping/ingestion layer, the search index,
//! and the "Add to OO" flow are later phases, not built here.

mod client;
mod config;

// Phase 0 only -- nothing in the crate imports these yet (Phase 1 wires
// up ingestion). Remove once a real caller exists.
#[allow(unused_imports)]
pub use client::{FormField, ProcessStreetClient, ProcessStreetError, Task, Workflow, WorkflowRun};
#[allow(unused_imports)]
pub use config::ProcessStreetConfig;
