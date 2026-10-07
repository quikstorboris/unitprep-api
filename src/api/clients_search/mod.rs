//! Search for a Process Street company/facility/person to import into
//! OO -- the entry point the "Add client from PS" flow (still Phase 3,
//! not built) will read from. Three genuinely different lookups running
//! side by side in one response:
//!
//! - **Facility matches**: a live call to PS's own server-side `name`
//!   filter over Intake runs only (`clients::search::search_by_facility_name`)
//!   -- cheap, no local index needed, always current. Each match also
//!   carries `already_imported`, a cheap local check against
//!   `clients.facilities.ps_intake_run_id` so a search result can be
//!   greyed out in the UI instead of silently inviting a duplicate
//!   import.
//! - **Merchant Account matches**: the same live server-side `name`
//!   search, over New Merchant Account run titles instead
//!   (`clients::search::search_by_merchant_account_name`). Added
//!   2026-09-14 after a real incident: MSS Jenks, LLC's real Elavon
//!   application (`rWwi2_88WoKj6C2qg8BG-g`) was invisible to this
//!   endpoint because it has no discoverable Intake run, and
//!   Intake-only search made a real, submitted application look like it
//!   didn't exist in PS at all (see `clients::search`'s own doc comment
//!   for the full story). **Not a facility identity on its own** -- an
//!   MA-only match carries `already_linked` (is this run already
//!   attached to some real facility via
//!   `clients.facility_merchant_accounts.ps_new_merchant_run_id`?)
//!   rather than `already_imported`, and the frontend must not offer to
//!   "Add" one directly: `clients::create` still requires an Intake run
//!   to build a real `clients.facilities` row from (address, PMS, etc.
//!   all come from Intake, never from Merchant Account). This list is
//!   for visibility/discovery -- confirming real PS data exists even
//!   when its Intake counterpart can't be found -- not an alternate
//!   import path.
//! - **Person matches**: a local query against `clients.ps_person_index`
//!   (`clients::sync`'s delta-synced projection) -- PS has no
//!   server-side search over form-field values, so this is the only way
//!   to find a facility by an owner/DM/manager/signer/POC's name. Only
//!   as fresh as the last sync (`clients.ps_sync_state.last_synced_at`
//!   per run), not live. **Not query-text-only**: once a facility is
//!   matched (by title, or by a person hit -- see below), every OTHER
//!   person already indexed on that same Intake run is folded in too,
//!   so finding a facility surfaces its full known contact list, not
//!   just whichever one person's name happened to contain the query.
//!   (Boris, 2026-09-03: searching a facility by name, or by only one
//!   of its several owners, must not leave its other owners "behind".)
//!
//! **A company name (e.g. "Prairie Enterprises") doesn't literally
//! appear in a facility's own Intake run title** (real title:
//! "Highway 20 Self Storage - QMS Onboarding") -- so a query for the
//! company name alone can hit zero facility matches even though every
//! sister facility is right there under a shared owner/DM. Rather than
//! indexing a separate "company name" field (PS doesn't structurally
//! expose one that's reliably distinct from a facility's own name --
//! see the vault's sister-site writeup), this reuses the person index
//! that already exists for exactly this shape of problem: any Intake
//! run reachable via a person-name/email hit on the same query is
//! folded into `facility_matches` too, tagged `matched_via: person`
//! (never silently merged with a real title hit -- the UI must be able
//! to show *why* a result showed up). This is the same mechanism a
//! sister-facility suggestion ("you found Highway 20, here are its
//! likely sisters") would need, so it's built once, generically, keyed
//! off the search query itself rather than a specific already-selected
//! facility.
//!
//! Requires only authentication, not a particular permission -- same
//! reasoning as `client_ops_qms_tags::list_qms_tags`: this is read-only
//! discovery data (facility/person names), not a client operation in
//! its own right.

mod display;
mod dto;
mod handler;
mod lookup;
mod matching;

#[cfg(test)]
pub use dto::SearchClientsQuery;
pub use handler::search_clients;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod lookup_db_tests;
