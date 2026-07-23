//! Core domain types: tenant records and the results of grouping/
//! comparing/flagging them. The field taxonomy (which columns exist,
//! their categories, normalization kind) lives in `types::fields` — a
//! separable concept from the record/result types below, split out
//! once this file crossed the project's 250-line flag point.

use serde::Serialize;

mod fields;
pub use fields::{FieldCategory, FieldKind, FieldName, FieldSpec, CATEGORY_PRIORITY, FIELD_SPECS};

/// One tenant/unit row from a QMS End Users export. Only the columns
/// this crate's logic actually reads — the full 71-column passthrough
/// needed for the eventual export artifact is an API/export-layer
/// concern, not this crate's (see project memory: output format is an
/// unresolved product decision).
#[derive(Debug, Clone, Default, Serialize)]
pub struct TenantRecord {
    /// Row identity, not used in matching logic itself — carried through
    /// for display and for the future "re-check two pulls over time"
    /// feature (undecided whether that's in v1).
    pub cust_numb: String,
    pub unit_number: String,

    /// The `FirtLast` column — pass-1 grouping key (trim+lowercase, see
    /// `grouping::group_key`). Named to match the source column, not
    /// reformatted, since this is a pre-existing upstream artifact this
    /// crate reads but never computes.
    pub first_last: String,

    pub first_name: String,
    pub last_name: String,
    pub company_name: String,

    pub phone_number: String,
    pub phone_number_prefix: String,
    pub email: String,

    pub address_street1: String,
    pub address_street2: String,
    pub address_city: String,
    pub address_state: String,
    pub address_postal_code: String,

    pub alt_contact_first_name: String,
    pub alt_contact_last_name: String,
    pub alt_contact_email: String,
    pub alt_contact_phone_number: String,
    pub alt_contact_phone_number_prefix: String,
    pub alt_contact_address_street1: String,
    pub alt_contact_address_street2: String,
    pub alt_contact_address_city: String,
    pub alt_contact_address_state: String,
    pub alt_contact_address_postal_code: String,
}

impl TenantRecord {
    /// Looks up a field's raw (unnormalized) value by name. The single
    /// place that maps `FieldName` to a struct field, so `FIELD_SPECS`
    /// and this can never silently drift apart the way two independent
    /// header normalizers once did elsewhere in UnitPrep.
    pub fn field(&self, name: FieldName) -> &str {
        match name {
            FieldName::PhoneNumber => &self.phone_number,
            FieldName::PhoneNumberPrefix => &self.phone_number_prefix,
            FieldName::Email => &self.email,
            FieldName::AddressStreet1 => &self.address_street1,
            FieldName::AddressStreet2 => &self.address_street2,
            FieldName::AddressCity => &self.address_city,
            FieldName::AddressState => &self.address_state,
            FieldName::AddressPostalCode => &self.address_postal_code,
            FieldName::AltContactFirstName => &self.alt_contact_first_name,
            FieldName::AltContactLastName => &self.alt_contact_last_name,
            FieldName::AltContactEmail => &self.alt_contact_email,
            FieldName::AltContactPhoneNumber => &self.alt_contact_phone_number,
            FieldName::AltContactPhoneNumberPrefix => &self.alt_contact_phone_number_prefix,
            FieldName::AltContactAddressStreet1 => &self.alt_contact_address_street1,
            FieldName::AltContactAddressStreet2 => &self.alt_contact_address_street2,
            FieldName::AltContactAddressCity => &self.alt_contact_address_city,
            FieldName::AltContactAddressState => &self.alt_contact_address_state,
            FieldName::AltContactAddressPostalCode => &self.alt_contact_address_postal_code,
            FieldName::CompanyName => &self.company_name,
            FieldName::FirstName => &self.first_name,
            FieldName::LastName => &self.last_name,
        }
    }

    /// "FirstName LastName" (falling back to `FirtLast` when both name
    /// parts are blank), Title Cased for display — the reference
    /// script's own version uppercases this, which reads as shouting in
    /// a note meant for a non-technical facility manager to read.
    pub fn display_name(&self) -> String {
        let name = format!("{} {}", self.first_name.trim(), self.last_name.trim());
        let name = name.trim();
        if name.is_empty() {
            title_case(self.first_last.trim())
        } else {
            title_case(name)
        }
    }
}

/// Capitalizes the first letter of each whitespace-separated word,
/// lowercasing the rest — good enough for tenant names (which arrive
/// with inconsistent casing across a real export) without needing a
/// full locale-aware title-casing dependency.
fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One field that differed within a group, and the actual values
/// involved — not just that it differed. Raw (trimmed, not
/// normalized) values are kept here specifically so a human — or,
/// later, an automated note composer — can say *what* differs
/// ("dcravalho@CHPMAUI.COM vs. blank"), not just *that* something does.
#[derive(Debug, Clone, Serialize)]
pub struct FieldValueMismatch {
    pub field: FieldName,
    /// Distinct raw values seen across the group for this field, in
    /// the reference script's own console-summary style: blank shown
    /// as `"(blank)"`, sorted with blank last.
    pub values: Vec<String>,
}

/// A category that differed within a group, and which specific fields
/// under it differed (for the detail lines under a flagged group).
#[derive(Debug, Clone, Serialize)]
pub struct FieldMismatch {
    pub category: FieldCategory,
    pub fields: Vec<FieldValueMismatch>,
}

/// A tenant's records grouped by exact `FirtLast` match (pass 1). Only
/// groups with 2+ records are ever produced downstream.
#[derive(Debug, Clone, Serialize)]
pub struct TenantGroup {
    pub key: String,
    pub records: Vec<TenantRecord>,
}

/// A multi-unit group with at least one contact-info disagreement —
/// the output of pass 2/3 (comparison + note assignment).
#[derive(Debug, Clone, Serialize)]
pub struct FlaggedGroup {
    pub group: TenantGroup,
    pub mismatches: Vec<FieldMismatch>,
    pub note: String,
}

/// A candidate pair of tenants whose display names are similar enough
/// to be the same tenant recorded under a typo/variant spelling. Under
/// this crate's always-flag policy, every candidate above the
/// surfacing threshold is reported here for human confirmation — none
/// are merged automatically, regardless of how high the ratio is
/// (unlike the reference script, which auto-merges ratio >= 0.90).
#[derive(Debug, Clone, Serialize)]
pub struct TypoVariantCandidate {
    pub key_a: String,
    pub key_b: String,
    pub ratio: f64,
    /// Whether every non-name contact field already matches across the
    /// two tenants' combined records — determines which of the two
    /// VERIFY_* notes is used, not whether this candidate gets
    /// surfaced (everything above threshold is surfaced either way).
    pub contact_info_matches: bool,

    /// `notes::NOTE_VERIFY_MATCHES` or `notes::NOTE_VERIFY_DIFFERS`,
    /// chosen by `contact_info_matches`. The reference script only
    /// attaches this note to pairs it decides to auto-merge; since this
    /// crate always surfaces every candidate above threshold, every
    /// candidate gets a note.
    pub note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(first: &str, last: &str, first_last: &str) -> TenantRecord {
        TenantRecord {
            first_name: first.to_string(),
            last_name: last.to_string(),
            first_last: first_last.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn display_name_title_cases_first_and_last_name() {
        let r = record("JOHN", "hawkins", "");
        assert_eq!(r.display_name(), "John Hawkins");
    }

    #[test]
    fn display_name_falls_back_to_title_cased_first_last_when_name_parts_are_blank() {
        let r = record("", "", "WILLIAMS OIL");
        assert_eq!(r.display_name(), "Williams Oil");
    }

    #[test]
    fn display_name_handles_already_mixed_case_input() {
        let r = record("Michelle", "Rodgers", "");
        assert_eq!(r.display_name(), "Michelle Rodgers");
    }
}
