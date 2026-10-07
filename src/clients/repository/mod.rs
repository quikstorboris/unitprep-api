//! Writes already-mapped Process Street data into the `clients` schema.
//! Every write runs against a caller-supplied RLS transaction (see
//! `auth::authenticated_user::begin_rls_transaction`) -- the same
//! mechanism every other write in this app uses, so these rows are
//! subject to the real onboarding_manager/department_manager RLS
//! gating, never a privileged bypass. Callers commit or roll back the
//! transaction themselves.
//!
//! Plain dynamic `sqlx::query`/`query_as`, matching this codebase's
//! established convention -- there is no compile-time-checked
//! `query!`/`query_as!` usage anywhere in this crate (no `.sqlx` offline
//! cache exists), so this file doesn't introduce one either.
//!
//! Takes only already-mapped, already-encrypted data. This module never
//! imports `FacilitySecrets`/`PartyPii` (private to
//! `merchant_account_mapping`) and never sees a plaintext SSN -- it
//! only ever binds the `Vec<u8>` ciphertext `encrypted_pii`/
//! `encrypted_secrets` already produced by that module.

mod contract_order;
mod facility;
mod merchant_account;
mod people;
mod task_status;

pub use facility::{insert_company, insert_facility, insert_facility_policies_and_people};
pub use merchant_account::{
    ingest_merchant_account_run, resync_merchant_account_run, IngestMerchantAccountError,
};
pub use people::{
    edit_person_and_facility_link, heal_person_in_place, unlink_person_from_facility,
    upsert_person_and_link_to_facility,
};
pub use task_status::upsert_task_status;

#[cfg(test)]
mod tests;
