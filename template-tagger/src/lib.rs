//! Core matching logic for the QMS Template Tagging Assistant, Phase 2
//! (rule-based candidate detection). Given a client document's plain
//! text and a set of known values, finds every literal occurrence of a
//! value and proposes the matching QMS merge tag as a substitution.
//!
//! Domain logic only -- no session state, no HTTP layer, no `.docx`
//! parsing. Depends on nothing but `std`, per the workspace's
//! established "new tool = new crate" pattern (see `unitprep-dedup`,
//! `unitprep-unit-group`).
//!
//! **Scope, locked 2026-08-10** (see the vault's tags-effort notes):
//! this crate is only ever meant to be called with tag values whose
//! meaning is unambiguous regardless of document context --
//! name/address/phone/email/DL#/unit number, the confirmed "easy cases"
//! from the original design note. The caller is responsible for
//! excluding any tag with a known context-dependent duality (the
//! `m.*`/`l.*` move-in-vs-lease pair, the `d.*` Date-vs-Delinquency
//! overload) until a real disambiguation mechanism exists -- this crate
//! has no notion of "document context" at all, by design. It is a pure
//! text-matching engine, not a QMS domain model.
//!
//! **Hard rule this crate exists to serve: propose, never modify.**
//! Every [`detect::Candidate`] returned here is a suggestion for a human
//! to confirm. Nothing calling this crate may apply a substitution
//! without review -- there is no function anywhere in this crate that
//! writes to a document.

pub mod detect;

pub use detect::{detect_candidates, Candidate, TagValue};
