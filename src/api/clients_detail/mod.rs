//! Read endpoints behind Phase 4's Client record UI -- the Company page
//! (sections 1-3 of the vault's own design note: Company Information,
//! Financial Information, Owner(s) Information, plus the facility-
//! selector rail) and a facility's own General tab + Facility Policies
//! tab. Read-only, same "any authenticated caller" gate `clients_search`/
//! `clients_preview` already use -- the genuinely sensitive parts
//! (Elavon activity, owner PII) are protected by RLS itself
//! (`facility_merchant_accounts`/`facility_merchant_account_parties` are
//! `onboarding_manager`/`department_manager`-only at the database level),
//! so a caller without that role simply gets those fields back empty
//! rather than needing a second permission check duplicated here.
//!
//! **Scoped to display only, this pass** -- no update/edit endpoints
//! yet. The vault's "global edit convention" (per-section Edit button,
//! everything editable except Elavon credentials) is real future work,
//! sequenced after read access exists to build against.

mod company;
mod facility;
mod policies;
mod policy_dto;
mod policy_queries;
#[cfg(test)]
mod tests;

pub use company::get_company_detail;
pub use facility::get_facility_detail;
pub use policies::get_facility_policies;
