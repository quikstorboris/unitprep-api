//! Mapping a Process Street New Merchant Account run's fields into a `MappedMerchantAccount` and its parties.

use super::all_sensitive_keys;
use super::secrets::{mask_bank_number, FacilitySecrets, PartyPii};
use crate::clients::encryption::{self, EncryptionError};
use crate::clients::fields::{value_for, value_for_any};
use crate::process_street::FormField;
use serde_json::Value;
use uuid::Uuid;

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
    pub(super) pii: PartyPii,
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

    pub(super) fn has_any_data(&self) -> bool {
        self.display_name.is_some()
            || self.title.is_some()
            || self.ownership_percent.is_some()
            || self.email.is_some()
            || self.phone.is_some()
            || !self.pii.is_empty()
    }
}

pub(super) fn parse_percent(raw: Option<String>) -> Option<f64> {
    raw?.parse::<f64>().ok()
}

pub(super) fn map_signer(fields: &[FormField]) -> MappedParty {
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

pub(super) fn map_owner(fields: &[FormField], index: i32) -> MappedParty {
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
pub(super) fn map_intermediary_business(fields: &[FormField], index: i32) -> MappedParty {
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

pub(super) fn map_parties(fields: &[FormField]) -> Vec<MappedParty> {
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
    /// PS's own `Legal_Name?` -- a yes/no-shaped select, real observed
    /// values `"Same as Business DBA"` / `"Different than Business
    /// DBA"`. When it's "same", PS's own form never asks `Legal_Name_2`
    /// at all (the AM doesn't retype the DBA into a second box), so
    /// `legal_name` above comes back `None` even though a real legal
    /// name -- `business_dba` -- is known; `clients::company_naming`
    /// consults this field to catch that case instead of falling all
    /// the way through to Intake's own (often blank, for a non-"first
    /// time" facility) legal name. Kept as raw text, not a bool, same
    /// Phase 1 convention as `ownership_type` below.
    pub legal_name_same_as_dba: Option<String>,
    /// PS's own `Ownership_Type` (e.g. "LLC", "Sole Proprietorship" --
    /// real observed value so far is just "LLC", so this is kept as
    /// raw text, not a Rust enum, the same Phase 1 convention as every
    /// other Facility-Policies-adjacent field).
    pub ownership_type: Option<String>,
    /// The revenue/volume fields Facility Information (Pre-App) asks for
    /// underwriting purposes -- confirmed genuinely per-facility (Prairie
    /// Enterprises' 3 real facilities each answered these differently on
    /// their own separate runs), so these live on the facility, not the
    /// company. Never in `SENSITIVE_FACILITY_KEYS` -- these already sat
    /// in plaintext in `sanitized_snapshot`/`raw_ps_snapshot`, just never
    /// promoted to a named column or shown in any UI before 2026-09-03.
    /// Raw text, not decimals -- same Phase 1 convention as every other
    /// PS-sourced financial field in this schema.
    pub total_annual_business_revenue_raw: Option<String>,
    pub total_monthly_sales_raw: Option<String>,
    pub average_credit_card_payment_amount_raw: Option<String>,
    pub highest_credit_card_payment_amount_raw: Option<String>,
    pub high_cc_payment_times_per_year_raw: Option<String>,
    pub offers_ach_raw: Option<String>,
    pub annual_electronic_check_volume_raw: Option<String>,
    pub average_electronic_check_amount_raw: Option<String>,
    pub maximum_electronic_check_amount_raw: Option<String>,
    pub parties: Vec<MappedParty>,
    /// A masked (last-4-only, via `mask_bank_number`) view of this run's
    /// own `EIN` -- exists purely so two candidate runs matching the
    /// same fuzzy facility name can be told apart (search results,
    /// "Potential Duplicates") without widening who can see a real tax
    /// ID. The unmasked value only ever exists inside `secrets`
    /// (encrypted at rest); this field is derived independently, not
    /// unmasked-then-masked, so the raw EIN never even transits through
    /// this struct's own public surface.
    pub ein_last_4: Option<String>,
    /// This run's own business street address (`Business_Address` +
    /// `City`/`State`/`Zip`, comma-joined onto one line), e.g. "84097
    /// Hwy 11, Milton Freewater, OR 97862" -- not previously mapped
    /// anywhere. Added 2026-09-23 alongside `ein_last_4`, for the same
    /// disambiguation reason: a real address is a much stronger signal
    /// than the title-text/DBA correlation already uses, confirmed
    /// against the Knapp's Self Stor of Milton Freewater / "Milton Self
    /// Storage" mix-up (one had a real filled address, the other had
    /// none at all).
    pub business_address: Option<String>,
    pub sanitized_snapshot: Value,
    pub(super) secrets: FacilitySecrets,
}

/// Joins a business address's separate PS fields onto one display line,
/// e.g. `("84097 Hwy 11", "Milton Freewater", "OR", "97862")` ->
/// `"84097 Hwy 11, Milton Freewater, OR 97862"`. `None` when every part
/// is missing -- a genuinely blank address (e.g. the "Milton Self
/// Storage" run) must stay `None`, not become an empty string that
/// looks like a real (if terse) answer.
pub(super) fn combine_address(
    street: Option<String>,
    city: Option<String>,
    state: Option<String>,
    zip: Option<String>,
) -> Option<String> {
    let state_zip = [state, zip]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let city_state_zip = [city, (!state_zip.is_empty()).then_some(state_zip)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");

    let parts: Vec<String> = [
        street,
        (!city_state_zip.is_empty()).then_some(city_state_zip),
    ]
    .into_iter()
    .flatten()
    .collect();

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(", "))
    }
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
pub fn map_merchant_account_fields(fields: &[FormField]) -> MappedMerchantAccount {
    MappedMerchantAccount {
        rate_provided: value_for(
            fields,
            "What_Processing_Rates_did_you_provide_to_the_customer?",
        ),
        application_status: value_for(fields, "What_is_their_software_onboarding_status?"),
        legal_name: value_for(fields, "Legal_Name_2"),
        business_dba: value_for(fields, "Business_DBA"),
        legal_name_same_as_dba: value_for(fields, "Legal_Name?"),
        ownership_type: value_for(fields, "Ownership_Type"),
        total_annual_business_revenue_raw: value_for(fields, "Total_Annual_Business_Revenue"),
        total_monthly_sales_raw: value_for(fields, "Total_Monthly_Sales"),
        average_credit_card_payment_amount_raw: value_for(
            fields,
            "Average_credit_card_payment_amount",
        ),
        highest_credit_card_payment_amount_raw: value_for(
            fields,
            "Highest_credit_card_payment_amount",
        ),
        high_cc_payment_times_per_year_raw: value_for(
            fields,
            "#_times_per_year_for_the_high_CC_Payment",
        ),
        offers_ach_raw: value_for(fields, "Do_you_want_to_offer_ACH"),
        annual_electronic_check_volume_raw: value_for(fields, "Annual_Electronic_Check_Volume"),
        average_electronic_check_amount_raw: value_for(fields, "Average_Electronic_Check_Amount"),
        maximum_electronic_check_amount_raw: value_for(fields, "Maximum_Electronic_Check_Amount"),
        parties: map_parties(fields),
        ein_last_4: value_for(fields, "EIN").as_deref().map(mask_bank_number),
        business_address: combine_address(
            value_for(fields, "Business_Address"),
            value_for(fields, "City"),
            value_for(fields, "State"),
            value_for(fields, "Zip"),
        ),
        sanitized_snapshot: sanitize_fields_for_snapshot(fields),
        secrets: FacilitySecrets::from_fields(fields),
    }
}

/// `credentials_added_to_qms` isn't a form field anywhere on New
/// Merchant Account -- confirmed 2026-09-03, live against the real API
/// (`GET /workflow-runs/{id}/tasks`), after Boris flagged the Elavon
/// tab showing "No" for a facility whose credentials step he'd already
/// completed in PS. It's a checklist *task*, same `/tasks` shape
/// `ps_task_status` already tracks -- never a mismapped field.
///
/// Which task names count is data, not a constant (2026-10-06: the step
/// was renamed "Document Credentials" in new templates, with the old
/// "Add Credentials to QMS" left in place but hidden) -- `names` is
/// `ps_task_roles::QMS_CREDENTIALS_ROLE`'s current mapping, loaded by
/// the caller. See `ps_task_roles::role_is_satisfied` for the matching
/// rules (hidden tasks never count).
pub fn credentials_added_to_qms_from_tasks(
    tasks: &[crate::process_street::Task],
    names: &[String],
) -> bool {
    crate::clients::ps_task_roles::role_is_satisfied(tasks, names)
}
