//! Duplicate tenant check: flags multi-unit tenants whose contact info
//! disagrees across units, and surfaces likely typo/name-variant tenants
//! for human review. Ported from the `duplicate-tenant-check` reference
//! script (Python), verified against three real facility sample pairs.
//!
//! Domain logic only — no session state, no HTTP layer, no export
//! format. Depends only on `unitprep-core`, per the workspace's
//! established "new tool = new crate" pattern.
//!
//! **Policy**: always flag, never auto-merge — including the reference
//! script's own ">=90% similarity" tier, which the script writes directly
//! into its output. Every typo/name-variant candidate this crate finds is
//! surfaced for a human to confirm; nothing is merged automatically. This
//! keeps the tool aligned with UnitPrep's project-wide principle that
//! fuzzy similarity is advisory-only and never decides an outcome by
//! itself.

pub mod comparison;
pub mod grouping;
pub mod ingest;
pub mod normalization;
pub mod notes;
pub mod report;
pub mod similarity;
pub mod types;

pub use report::{run, DedupReport};
pub use types::TenantRecord;
