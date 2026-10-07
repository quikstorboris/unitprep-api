//! Maps a 💳 New Merchant Account run's form fields into the shapes
//! `clients::repository` writes to Postgres.
//!
//! **This workflow is the one place in Onboarding Orchestrator that
//! handles genuinely sensitive data**: SSN, date of birth, and home
//! address for the signer and every listed owner, plus EIN, bank
//! routing/account numbers, and QMS/processor system credentials. See
//! `clients::encryption`'s module doc for the full reasoning and the
//! vault's PII/compliance backlog item this resolves.
//!
//! The sensitive plaintext (`FacilitySecrets`, `PartyPii`) never leaves
//! this module unencrypted -- callers only ever get back already
//! -encrypted bytes (`MappedMerchantAccount::encrypted_secrets`,
//! `MappedParty::encrypted_pii`) and a `sanitized_snapshot` with every
//! sensitive key already stripped out. `clients::repository` never
//! imports `FacilitySecrets`/`PartyPii` and never sees a plaintext SSN.

mod mapping;
mod secrets;
#[cfg(test)]
mod tests;

pub use mapping::{
    credentials_added_to_qms_from_tasks, map_merchant_account_fields, MappedMerchantAccount,
    MappedParty,
};
pub use secrets::{
    decrypt_elavon_credentials, decrypt_facility_secrets, decrypt_party_pii, mask_bank_number,
};

/// Every PS field key this module treats as sensitive -- excluded from
/// `sanitized_snapshot` unconditionally, regardless of whether it also
/// gets encrypted elsewhere. This is the single source of truth for
/// "must never sit in plaintext JSONB" -- both the snapshot sanitizer
/// and the encrypted-bundle builders read from the same lists below, so
/// there is no way for a key to be forgotten from one but not the other.
pub(super) const SENSITIVE_FACILITY_KEYS: &[&str] = &[
    "EIN",
    "Bank_Routing_Number",
    "Bank_Account_Number",
    "QUIKSTOR_Password",
    "QSS_WEB_PIN",
    "Pinpad_User_ID",
    "QSS_API_Pin",
    "MID",
    "ACCOUNT_ID",
];

/// The 6 per-party fields that go in `PartyPii`, for every party prefix
/// PS's template defines (`Signer`, `Owner_1..4`; `Intermediary_Business_1..4`
/// have no equivalent individual fields at all, since a business has no
/// SSN/DOB/home address in this form).
pub(super) const PARTY_PII_SUFFIXES: &[&str] = &[
    "SSN",
    "DOB",
    "HOME_Address",
    "City",
    "State_or_Province",
    "Postal_Code",
];

pub(super) fn all_sensitive_keys() -> Vec<String> {
    let mut keys: Vec<String> = SENSITIVE_FACILITY_KEYS
        .iter()
        .map(|k| k.to_string())
        .collect();
    for prefix in party_prefixes() {
        for suffix in PARTY_PII_SUFFIXES {
            keys.push(format!("{prefix}_-_{suffix}"));
        }
    }
    keys
}

pub(super) fn party_prefixes() -> Vec<String> {
    let mut prefixes = vec!["Signer".to_string()];
    for i in 1..=4 {
        prefixes.push(format!("Owner_{i}"));
    }
    prefixes
}
