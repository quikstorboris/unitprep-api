//! Facility page's Users tab. `GET` returns two things: the facility's
//! actual saved roster (`clients.facility_people`/`clients.people`,
//! written once at ingest and never touched again automatically otherwise)
//! and "Add User" candidates -- rows already sitting in
//! `clients.ps_person_index` for this facility's own `ps_intake_run_id`,
//! kept fresh by the nightly background sync independent of when the
//! facility was created or last touched. No live Process Street call and
//! no search box: the candidates are exactly what PS currently says for
//! this facility's own Intake run, already indexed.
//!
//! `GET` also silently self-heals: any roster person whose stored
//! name/phone disagrees with a fresh candidate gets corrected in place
//! before the response goes out (see `repository::heal_person_in_place`'s
//! own doc comment for the real example, Sand-Sto's "Irene Chen - (301)
//! 787-9221"). No click needed -- viewing the tab is enough. This
//! replaced an earlier design (Boris, 2026-09-04) where a click on an
//! already-linked chip did the refresh; that click now means unlink
//! instead (see `DELETE` below), so the fix had to stop needing a click
//! at all.
//!
//! **Matching is by email+role, but a shared inbox can make that
//! genuinely ambiguous** (2026-09-08, real bug: Dubuqueland's Soppe
//! family -- several distinct people sharing one family email, all
//! "owner"). Self-heal prefers an exact name match among same-email,
//! same-role candidates; only falls back to a bare email+role match when
//! it's unambiguous (exactly one such candidate). Two or more
//! different-named candidates sharing that email+role is left alone
//! rather than guessed at -- see `get_facility_people`'s own candidate-
//! selection code below, and `repository::link_person_to_facility`'s doc
//! comment for why `clients.people` identity itself is now (email, name)
//! rather than email alone.
//!
//! `POST` adds a person -- either an "Add User" chip click for a
//! candidate not yet on the roster (`source: "process_street"`), or a
//! brand-new person typed in by hand (`source: "manual"`). A 'manual'
//! link is permanently excluded from the self-heal pass above -- see
//! `clients.facility_people.source`'s own migration comment -- the
//! "add a user that will never be overwritten by re-sync" Boris asked
//! for, 2026-09-08.
//!
//! `PUT .../people/{person_id}` is the Users tab's "Edit" action --
//! retyping a roster person's own name/email/phone/role directly. A
//! 'process_street' person's edit carries `protect_from_resync`: when
//! true, their link flips to 'manual' (so this edit itself survives the
//! next self-heal); when false, the edit is saved but the next self-heal
//! pass can still silently revert it back to whatever the index says --
//! deliberately risky, the same choice/warning Boris asked the Users tab
//! present at edit time rather than losing the edit invisibly later. A
//! 'manual' person's edit never asks -- it's already permanently exempt.
//!
//! `DELETE .../people/{person_id}?role=...` unlinks one roster entry --
//! the same chip, now rendered red for an already-linked candidate,
//! rather than a separate control.

mod add;
mod dto;
mod edit;
mod get;
mod owners;
#[cfg(test)]
mod tests;
mod unlink;

pub use add::add_facility_person;
pub use edit::edit_facility_person;
pub use get::get_facility_people;
pub use unlink::unlink_facility_person;

pub(super) const SOURCES: &[&str] = &["process_street", "manual"];
