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

// `clients::create::create_company_and_facilities` is a real caller as
// of 2026-09-03 (Elavon ingestion at Create time), but several helpers
// here (e.g. `sanitize_fields_for_snapshot` outside a specific call
// path) are still only exercised by this module's own tests -- kept
// broad rather than narrowed item-by-item for now.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::clients::encryption::{self, EncryptionError};
use crate::clients::fields::{value_for, value_for_any};
use crate::process_street::FormField;

/// Every PS field key this module treats as sensitive -- excluded from
/// `sanitized_snapshot` unconditionally, regardless of whether it also
/// gets encrypted elsewhere. This is the single source of truth for
/// "must never sit in plaintext JSONB" -- both the snapshot sanitizer
/// and the encrypted-bundle builders read from the same lists below, so
/// there is no way for a key to be forgotten from one but not the other.
const SENSITIVE_FACILITY_KEYS: &[&str] = &[
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
const PARTY_PII_SUFFIXES: &[&str] = &[
    "SSN",
    "DOB",
    "HOME_Address",
    "City",
    "State_or_Province",
    "Postal_Code",
];

fn all_sensitive_keys() -> Vec<String> {
    let mut keys: Vec<String> = SENSITIVE_FACILITY_KEYS.iter().map(|k| k.to_string()).collect();
    for prefix in party_prefixes() {
        for suffix in PARTY_PII_SUFFIXES {
            keys.push(format!("{prefix}_-_{suffix}"));
        }
    }
    keys
}

fn party_prefixes() -> Vec<String> {
    let mut prefixes = vec!["Signer".to_string()];
    for i in 1..=4 {
        prefixes.push(format!("Owner_{i}"));
    }
    prefixes
}

/// Facility-level secrets, encrypted as one JSON bundle bound to the
/// facility. Never `pub` outside this module.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct FacilitySecrets {
    ein: Option<String>,
    bank_routing_number: Option<String>,
    bank_account_number: Option<String>,
    quikstor_password: Option<String>,
    qss_web_pin: Option<String>,
    pinpad_user_id: Option<String>,
    qss_api_pin: Option<String>,
    mid: Option<String>,
    account_id: Option<String>,
}

impl FacilitySecrets {
    fn is_empty(&self) -> bool {
        self.ein.is_none()
            && self.bank_routing_number.is_none()
            && self.bank_account_number.is_none()
            && self.quikstor_password.is_none()
            && self.qss_web_pin.is_none()
            && self.pinpad_user_id.is_none()
            && self.qss_api_pin.is_none()
            && self.mid.is_none()
            && self.account_id.is_none()
    }

    fn from_fields(fields: &[FormField]) -> Self {
        Self {
            ein: value_for(fields, "EIN"),
            bank_routing_number: value_for(fields, "Bank_Routing_Number"),
            bank_account_number: value_for(fields, "Bank_Account_Number"),
            quikstor_password: value_for(fields, "QUIKSTOR_Password"),
            qss_web_pin: value_for(fields, "QSS_WEB_PIN"),
            pinpad_user_id: value_for(fields, "Pinpad_User_ID"),
            qss_api_pin: value_for(fields, "QSS_API_Pin"),
            mid: value_for(fields, "MID"),
            account_id: value_for(fields, "ACCOUNT_ID"),
        }
    }
}

/// One party's sensitive PII, encrypted as one JSON bundle bound to
/// that specific party (not just the facility) -- see
/// `clients::encryption`'s module doc on why the AAD goes this granular.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
struct PartyPii {
    ssn: Option<String>,
    dob: Option<String>,
    home_address_line1: Option<String>,
    home_city: Option<String>,
    home_state_or_province: Option<String>,
    home_postal_code: Option<String>,
}

impl PartyPii {
    fn is_empty(&self) -> bool {
        self.ssn.is_none()
            && self.dob.is_none()
            && self.home_address_line1.is_none()
            && self.home_city.is_none()
            && self.home_state_or_province.is_none()
            && self.home_postal_code.is_none()
    }

    fn from_fields(fields: &[FormField], prefix: &str) -> Self {
        Self {
            ssn: value_for(fields, &format!("{prefix}_-_SSN")),
            dob: value_for(fields, &format!("{prefix}_-_DOB")),
            home_address_line1: value_for(fields, &format!("{prefix}_-_HOME_Address")),
            home_city: value_for(fields, &format!("{prefix}_-_City")),
            home_state_or_province: value_for(fields, &format!("{prefix}_-_State_or_Province")),
            home_postal_code: value_for(fields, &format!("{prefix}_-_Postal_Code")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MappedParty {
    /// `signer` | `owner` | `intermediary_business` -- matches
    /// `facility_merchant_account_parties.party_role`'s CHECK constraint.
    pub party_role: &'static str,
    /// 0 for signer, 1-4 for owner/intermediary_business.
    pub party_index: i32,
    pub display_name: Option<String>,
    pub title: Option<String>,
    pub ownership_percent: Option<f64>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub country_of_citizenship: Option<String>,
    pub country: Option<String>,
    pii: PartyPii,
}

impl MappedParty {
    /// `None` if this party has no sensitive PII at all (every
    /// `intermediary_business` party, and any owner/signer slot PS left
    /// entirely blank) -- callers should leave `encrypted_pii` NULL in
    /// that case rather than encrypting an all-empty bundle.
    pub fn encrypted_pii(&self, facility_id: Uuid) -> Result<Option<Vec<u8>>, EncryptionError> {
        if self.pii.is_empty() {
            return Ok(None);
        }
        let aad = format!("{facility_id}:{}:{}", self.party_role, self.party_index);
        let plaintext = serde_json::to_vec(&self.pii)
            .expect("PartyPii serialization cannot fail -- no non-serializable types");
        encryption::encrypt(aad.as_bytes(), &plaintext).map(Some)
    }

    fn has_any_data(&self) -> bool {
        self.display_name.is_some()
            || self.title.is_some()
            || self.ownership_percent.is_some()
            || self.email.is_some()
            || self.phone.is_some()
            || !self.pii.is_empty()
    }
}

fn parse_percent(raw: Option<String>) -> Option<f64> {
    raw?.parse::<f64>().ok()
}

fn map_signer(fields: &[FormField]) -> MappedParty {
    let first = value_for(fields, "Signer_-_First_Name");
    let last = value_for(fields, "Signer_-_Last_Name");
    let display_name = match (first, last) {
        (Some(f), Some(l)) => Some(format!("{f} {l}")),
        (Some(f), None) => Some(f),
        (None, Some(l)) => Some(l),
        (None, None) => None,
    };
    MappedParty {
        party_role: "signer",
        party_index: 0,
        display_name,
        title: value_for(fields, "Signer_-_Title"),
        ownership_percent: parse_percent(value_for_any(
            fields,
            &[
                "Signer_-_%_Ownership_in_Business".to_string(),
                "Signer_-_%_Ownership_in_Buisness".to_string(),
            ],
        )),
        email: value_for(fields, "Signer_-_Email"),
        phone: value_for(fields, "Signer_-_Home_or_Cell_Phone"),
        country_of_citizenship: value_for(fields, "Signer_-_Country_of_Citizenship"),
        country: value_for(fields, "Signer_-_Country"),
        pii: PartyPii::from_fields(fields, "Signer"),
    }
}

fn map_owner(fields: &[FormField], index: i32) -> MappedParty {
    let prefix = format!("Owner_{index}");
    let first = value_for(fields, &format!("{prefix}_-_First_Name"));
    let last = value_for(fields, &format!("{prefix}_-_Last_Name"));
    let display_name = match (first, last) {
        (Some(f), Some(l)) => Some(format!("{f} {l}")),
        (Some(f), None) => Some(f),
        (None, Some(l)) => Some(l),
        (None, None) => None,
    };
    MappedParty {
        party_role: "owner",
        party_index: index,
        display_name,
        title: value_for(fields, &format!("{prefix}_-_Title")),
        // A real PS template typo: some owner slots spell this
        // "Buisness", others "Business" -- see value_for_any's doc.
        ownership_percent: parse_percent(value_for_any(
            fields,
            &[
                format!("{prefix}_-_%_Ownership_in_Business"),
                format!("{prefix}_-_%_Ownership_in_Buisness"),
            ],
        )),
        email: value_for(fields, &format!("{prefix}_-_Email")),
        phone: value_for(fields, &format!("{prefix}_-_Home_or_Cell_Phone")),
        country_of_citizenship: value_for(fields, &format!("{prefix}_-_Country_of_Citizenship")),
        country: value_for(fields, &format!("{prefix}_-_Country")),
        pii: PartyPii::from_fields(fields, &prefix),
    }
}

/// Intermediary businesses have no SSN/DOB/home-address fields in PS's
/// template -- only a business name and a contact person. The contact's
/// name is folded into `title` (as `"Contact: First Last"`) rather than
/// modeled as its own column, a deliberate simplification for this rare
/// edge case (zero real facilities examined this session had one) --
/// worth a real column if a facility that actually uses this ever needs
/// to search/filter on the contact name specifically.
fn map_intermediary_business(fields: &[FormField], index: i32) -> MappedParty {
    let prefix = format!("Intermediary_Business_{index}");
    let contact_first = value_for(fields, &format!("{prefix}_-_Contact_First_Name"));
    let contact_last = value_for(fields, &format!("{prefix}_-_Contact_Last_Name"));
    let title = match (contact_first, contact_last) {
        (Some(f), Some(l)) => Some(format!("Contact: {f} {l}")),
        (Some(f), None) => Some(format!("Contact: {f}")),
        (None, Some(l)) => Some(format!("Contact: {l}")),
        (None, None) => None,
    };
    MappedParty {
        party_role: "intermediary_business",
        party_index: index,
        display_name: value_for(fields, &format!("{prefix}_-_Name")),
        title,
        ownership_percent: parse_percent(value_for(fields, &format!("{prefix}_-_Ownership_%"))),
        email: value_for(fields, &format!("{prefix}_-_Email_Address")),
        phone: value_for(fields, &format!("{prefix}_-_Contact_Phone")),
        country_of_citizenship: None,
        country: None,
        pii: PartyPii::default(),
    }
}

fn map_parties(fields: &[FormField]) -> Vec<MappedParty> {
    let mut parties = vec![map_signer(fields)];
    for i in 1..=4 {
        parties.push(map_owner(fields, i));
    }
    for i in 1..=4 {
        parties.push(map_intermediary_business(fields, i));
    }
    parties.retain(MappedParty::has_any_data);
    parties
}

/// Builds the `raw_ps_snapshot` for the non-sensitive parts of a
/// Merchant Account run -- every field whose key is NOT in
/// `all_sensitive_keys()`, keyed by PS's own field key, value `{label,
/// value}`. This is the property the tests below hold to the highest
/// bar: every single sensitive key must be verifiably absent, not just
/// "usually" filtered.
pub fn sanitize_fields_for_snapshot(fields: &[FormField]) -> Value {
    let denylist = all_sensitive_keys();
    let mut map = serde_json::Map::new();
    for f in fields {
        if denylist.contains(&f.key) {
            continue;
        }
        map.insert(
            f.key.clone(),
            serde_json::json!({ "label": f.label, "value": f.value_as_str() }),
        );
    }
    Value::Object(map)
}

#[derive(Debug, Clone, PartialEq)]
pub struct MappedMerchantAccount {
    pub rate_provided: Option<String>,
    pub application_status: Option<String>,
    /// PS's own `Legal_Name_2` (Facility Information / Pre-App step) --
    /// preferred over Intake's own legal-name field when this run
    /// exists at all, per Boris's call: Elavon's own application asks
    /// this question more carefully than Intake does.
    pub legal_name: Option<String>,
    /// PS's own `Business_DBA` -- the operating/facility name half of
    /// the sole-proprietor naming rule (`clients::company_naming`).
    pub business_dba: Option<String>,
    /// PS's own `Ownership_Type` (e.g. "LLC", "Sole Proprietorship" --
    /// real observed value so far is just "LLC", so this is kept as
    /// raw text, not a Rust enum, the same Phase 1 convention as every
    /// other Facility-Policies-adjacent field).
    pub ownership_type: Option<String>,
    pub parties: Vec<MappedParty>,
    pub sanitized_snapshot: Value,
    secrets: FacilitySecrets,
}

impl MappedMerchantAccount {
    /// `None` if nothing sensitive was ever answered on this run.
    pub fn encrypted_secrets(&self, facility_id: Uuid) -> Result<Option<Vec<u8>>, EncryptionError> {
        if self.secrets.is_empty() {
            return Ok(None);
        }
        let plaintext = serde_json::to_vec(&self.secrets)
            .expect("FacilitySecrets serialization cannot fail -- no non-serializable types");
        encryption::encrypt(facility_id.as_bytes(), &plaintext).map(Some)
    }
}

/// Decrypted view of a party's PII, for the Company page's "Owner(s)
/// Information" section (Phase 4) -- the read counterpart to
/// `MappedParty::encrypted_pii`. A public, standalone mirror of the
/// private write-time `PartyPii` shape (same JSON field names, since
/// they're the same bytes) rather than a `pub use` of it, so the
/// write-time struct's own serde attributes can keep evolving
/// independently of what this read path promises callers.
#[derive(Debug, Clone, Deserialize)]
pub struct DecryptedPartyPii {
    pub ssn: Option<String>,
    pub dob: Option<String>,
    pub home_address_line1: Option<String>,
    pub home_city: Option<String>,
    pub home_state_or_province: Option<String>,
    pub home_postal_code: Option<String>,
}

/// Decrypts one party's `encrypted_pii` blob -- `aad` must be built the
/// exact same way `MappedParty::encrypted_pii` built it at write time
/// (`"{facility_id}:{party_role}:{party_index}"`), or decryption fails
/// by design (see that method's own doc comment on why the AAD is bound
/// this granularly).
pub fn decrypt_party_pii(
    facility_id: Uuid,
    party_role: &str,
    party_index: i32,
    blob: &[u8],
) -> Result<DecryptedPartyPii, EncryptionError> {
    let aad = format!("{facility_id}:{party_role}:{party_index}");
    let plaintext = encryption::decrypt(aad.as_bytes(), blob)?;
    serde_json::from_slice(&plaintext).map_err(|_| EncryptionError::Undecryptable("malformed PartyPii plaintext"))
}

pub fn map_merchant_account_fields(fields: &[FormField]) -> MappedMerchantAccount {
    MappedMerchantAccount {
        rate_provided: value_for(fields, "What_Processing_Rates_did_you_provide_to_the_customer?"),
        application_status: value_for(fields, "What_is_their_software_onboarding_status?"),
        legal_name: value_for(fields, "Legal_Name_2"),
        business_dba: value_for(fields, "Business_DBA"),
        ownership_type: value_for(fields, "Ownership_Type"),
        parties: map_parties(fields),
        sanitized_snapshot: sanitize_fields_for_snapshot(fields),
        secrets: FacilitySecrets::from_fields(fields),
    }
}

/// `credentials_added_to_qms` isn't a form field anywhere on New
/// Merchant Account -- confirmed 2026-09-03, live against the real API
/// (`GET /workflow-runs/{id}/tasks`), after Boris flagged the Elavon
/// tab showing "No" for a facility whose "Add Credentials to QMS" step
/// he'd already completed in PS. It's a checklist *task*, same
/// `/tasks` shape `ps_task_status` already tracks for Intake -- this
/// facility never had it checked at all (`ingest_merchant_account_run`
/// never took a value for the column, so every real row defaulted to
/// the schema's own `false`), not a mismapped field.
pub fn credentials_added_to_qms_from_tasks(tasks: &[crate::process_street::Task]) -> bool {
    tasks.iter().any(|task| task.name.trim().eq_ignore_ascii_case("Add Credentials to QMS") && task.status == "Completed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // Real PS field keys/shapes from Prairie Enterprises' Highway 20
    // New Merchant Account run, with every sensitive value (SSN, DOB,
    // home address, EIN, bank routing/account, QMS system credentials)
    // replaced by obvious fakes before this file was ever written to
    // disk -- see the vault's Process Street schema doc for how this
    // fixture was sanitized. Non-sensitive values (business address,
    // rate, application status, real annotation formats) are real.
    const HIGHWAY20_NMA_FIELDS_SANITIZED: &str =
        include_str!("testdata/highway20_merchant_account_fields_sanitized.json");

    fn real_fields() -> Vec<FormField> {
        serde_json::from_str(HIGHWAY20_NMA_FIELDS_SANITIZED)
            .expect("fixture must parse as Vec<FormField>")
    }

    fn task(name: &str, status: &str) -> crate::process_street::Task {
        crate::process_street::Task { id: "t1".to_string(), name: name.to_string(), status: status.to_string() }
    }

    #[test]
    fn credentials_added_to_qms_from_tasks_is_true_when_that_task_is_completed() {
        let tasks = vec![task("Facility Information (Pre-App)", "Completed"), task("Add Credentials to QMS", "Completed")];
        assert!(credentials_added_to_qms_from_tasks(&tasks));
    }

    #[test]
    fn credentials_added_to_qms_from_tasks_is_false_when_that_task_is_not_completed() {
        let tasks = vec![task("Add Credentials to QMS", "NotCompleted")];
        assert!(!credentials_added_to_qms_from_tasks(&tasks));
    }

    #[test]
    fn credentials_added_to_qms_from_tasks_is_false_when_the_task_is_absent() {
        let tasks = vec![task("Facility Information (Pre-App)", "Completed")];
        assert!(!credentials_added_to_qms_from_tasks(&tasks));
    }

    #[test]
    fn credentials_added_to_qms_from_tasks_matches_case_insensitively() {
        let tasks = vec![task("add credentials to qms", "Completed")];
        assert!(credentials_added_to_qms_from_tasks(&tasks));
    }

    fn set_test_key() {
        std::env::set_var(
            "CLIENT_PII_ENCRYPTION_KEY",
            "1111111111111111111111111111111111111111111111111111111111111111",
        );
    }
    fn clear_test_key() {
        std::env::remove_var("CLIENT_PII_ENCRYPTION_KEY");
    }

    #[test]
    fn sanitized_snapshot_never_contains_any_sensitive_key_or_its_fake_value() {
        let mapped = map_merchant_account_fields(&real_fields());
        let snapshot_text = mapped.sanitized_snapshot.to_string();

        for key in all_sensitive_keys() {
            assert!(
                !mapped.sanitized_snapshot.as_object().unwrap().contains_key(&key),
                "sanitized snapshot must never contain the sensitive key {key}"
            );
        }
        // Even the fake placeholder values used in this fixture must
        // never appear -- proves the values were dropped along with
        // their keys, not just renamed.
        assert!(!snapshot_text.contains("FakeTestPassword123"));
        assert!(!snapshot_text.contains("1 Fake Test Lane"));
        assert!(!snapshot_text.contains("000000000")); // fake SSN/EIN/MID
    }

    #[test]
    fn maps_legal_name_dba_and_ownership_type_from_the_real_pre_app_fields() {
        let mapped = map_merchant_account_fields(&real_fields());
        assert_eq!(mapped.legal_name.as_deref(), Some("Prairie Enterprises LLC"));
        assert_eq!(mapped.business_dba.as_deref(), Some("Highway 20 self storage"));
        assert_eq!(mapped.ownership_type.as_deref(), Some("LLC"));
    }

    #[test]
    fn sanitized_snapshot_keeps_non_sensitive_business_data() {
        let mapped = map_merchant_account_fields(&real_fields());
        let snapshot_text = mapped.sanitized_snapshot.to_string();
        assert!(snapshot_text.contains("Prairie Enterprises"));
        assert!(mapped
            .sanitized_snapshot
            .as_object()
            .unwrap()
            .contains_key("Business_DBA"));
    }

    #[test]
    fn maps_three_real_owners_and_skips_the_blank_fourth_and_signer() {
        let mapped = map_merchant_account_fields(&real_fields());
        let owners: Vec<_> = mapped.parties.iter().filter(|p| p.party_role == "owner").collect();
        assert_eq!(owners.len(), 3, "owner 4 was blank on this real run and must be skipped");
        assert_eq!(owners[0].display_name.as_deref(), Some("Kyle Lindley"));
        assert_eq!(owners[0].ownership_percent, Some(30.0));

        assert!(
            !mapped.parties.iter().any(|p| p.party_role == "signer"),
            "the signer slot was entirely blank on this real run and must be skipped"
        );
    }

    #[test]
    #[serial(client_pii_encryption_key_env)]
    fn encrypts_and_round_trips_a_real_owners_pii() {
        set_test_key();
        let mapped = map_merchant_account_fields(&real_fields());
        let owner = mapped
            .parties
            .iter()
            .find(|p| p.party_role == "owner" && p.party_index == 1)
            .unwrap();

        let facility_id = Uuid::new_v4();
        let blob = owner
            .encrypted_pii(facility_id)
            .expect("encryption must succeed")
            .expect("owner 1 has real PII on this fixture");

        let aad = format!("{facility_id}:owner:1");
        let decrypted = encryption::decrypt(aad.as_bytes(), &blob).expect("decryption must succeed");
        let pii: PartyPii = serde_json::from_slice(&decrypted).unwrap();
        assert_eq!(pii.ssn.as_deref(), Some("000000000")); // the fixture's fake SSN
        clear_test_key();
    }

    #[test]
    #[serial(client_pii_encryption_key_env)]
    fn a_partys_pii_does_not_decrypt_under_a_different_partys_aad() {
        set_test_key();
        let mapped = map_merchant_account_fields(&real_fields());
        let owner1 = mapped.parties.iter().find(|p| p.party_index == 1 && p.party_role == "owner").unwrap();

        let facility_id = Uuid::new_v4();
        let blob = owner1.encrypted_pii(facility_id).unwrap().unwrap();

        let wrong_aad = format!("{facility_id}:owner:2");
        assert!(
            encryption::decrypt(wrong_aad.as_bytes(), &blob).is_err(),
            "owner 1's ciphertext must not decrypt under owner 2's AAD, even within the same facility"
        );
        clear_test_key();
    }

    #[test]
    fn intermediary_business_parties_have_no_encrypted_pii() {
        let mapped = map_merchant_account_fields(&real_fields());
        let biz = mapped
            .parties
            .iter()
            .find(|p| p.party_role == "intermediary_business")
            .expect("this fixture has one real intermediary business");
        assert_eq!(biz.encrypted_pii(Uuid::new_v4()).unwrap(), None);
    }

    #[test]
    #[serial(client_pii_encryption_key_env)]
    fn facility_secrets_encrypt_and_round_trip() {
        set_test_key();
        let mapped = map_merchant_account_fields(&real_fields());
        let facility_id = Uuid::new_v4();
        let blob = mapped
            .encrypted_secrets(facility_id)
            .expect("encryption must succeed")
            .expect("this fixture has real (fake-value) secrets");

        let decrypted = encryption::decrypt(facility_id.as_bytes(), &blob).unwrap();
        let secrets: FacilitySecrets = serde_json::from_slice(&decrypted).unwrap();
        assert_eq!(secrets.ein.as_deref(), Some("111111111"));
        assert_eq!(secrets.quikstor_password.as_deref(), Some("FakeTestPassword123!"));
        clear_test_key();
    }
}
