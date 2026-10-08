//! Facility page's Elavon tab -- Phase 4 item 5. `GET` shows whichever
//! is true: this facility already has a linked New Merchant Account
//! run (its own summary + parties, matching Company page's Owner(s)
//! Information but scoped to this one facility), or it doesn't, in
//! which case a title-correlation candidate is suggested the same way
//! `api::clients_search`/`api::clients_preview` already do it for a
//! not-yet-imported facility. `POST .../link` is the manual confirm
//! action for that candidate (or any run id the caller already knows) --
//! this is the general-purpose fix for what created Prairie/Highway
//! 20's own gap (2026-09-03): a facility created before its Merchant
//! Account run was ever correlated, or where auto-correlation simply
//! never found a match, previously had no way to get linked at all
//! short of a one-off backfill script. This tab is that path, built to
//! be used repeatedly, not just once.
//!
//! Deliberate friction, per the original design: the candidate is shown
//! with its own run name and PS run id so the caller can go verify it
//! in Process Street before confirming -- `link_facility_elavon` never
//! runs on its own, only on an explicit id the caller (or the candidate
//! suggestion) already named.

mod build;
mod dto;
mod get;
mod link;
mod resync;
#[cfg(test)]
mod tests;
mod unlink;

pub use get::get_facility_elavon;
pub use link::link_facility_elavon;
#[cfg(test)]
pub use link::LinkElavonRequest;
pub use resync::resync_elavon_data;
pub use unlink::unlink_facility_elavon;

use axum::response::Response;

use crate::api::conflict;

pub(super) const PERMISSION: &str = "client_ops.perform";

pub(super) fn already_linked() -> Response {
    conflict(
        "already_linked",
        "This facility already has a linked Merchant Account run.".to_string(),
    )
}

pub(super) fn not_linked() -> Response {
    conflict(
        "not_linked",
        "This facility has no linked Merchant Account run yet.".to_string(),
    )
}
