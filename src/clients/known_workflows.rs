//! The three Process Street workflow template ids this integration
//! cares about, out of the ~90 templates in the real org. See
//! [[Process Street Integration — Kickoff & Findings]] in the vault for
//! why these three and not others.

// No HTTP handler or CLI binary calls into `clients::search` (this
// module's only caller) yet. Remove once one exists.
#![allow(dead_code)]

/// 🚂 Intake / Progress -- client info collection, one run per facility.
pub const INTAKE_WORKFLOW_ID: &str = "tRh93HgRC5OLom3UxhJD3w";

/// 💳 New Merchant Account -- one run per facility, always 1:1.
pub const MERCHANT_ACCOUNT_WORKFLOW_ID: &str = "rhUaJ-KRu0ejEOYQ-jxGMA";

/// ✅ Contract Order. Ignore the duplicate old template
/// `Contract Order (OLD WAY 01/29/25)` -- not tracked here since nothing
/// should ever search or ingest against it.
pub const CONTRACT_ORDER_WORKFLOW_ID: &str = "j_idx2uXcI0_6gs4XvZGBA";
