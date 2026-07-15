//! Core domain types. Field names, categories, and priority order mirror
//! the reference script's `FIELD_CATEGORIES` (as of the 2026-07-14
//! revision: `CompanyName` has its own category, split out from `name`).

/// A contact-info category a tenant group can disagree on. Declared in
/// the exact priority order the reference script uses to pick which
/// note to show when multiple categories differ at once (first match
/// in this order wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldCategory {
    Phone,
    Email,
    Address,
    AltContact,
    Company,
    Name,
}

/// Priority order for note selection — earlier entries win when more
/// than one category differs within a group.
pub const CATEGORY_PRIORITY: [FieldCategory; 6] = [
    FieldCategory::Phone,
    FieldCategory::Email,
    FieldCategory::Address,
    FieldCategory::AltContact,
    FieldCategory::Company,
    FieldCategory::Name,
];

/// Whether a field's value needs address-specific normalization
/// (street-suffix/direction lookup, period-stripping) or just
/// case/whitespace normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Plain,
    Address,
}

/// Every QMS export column this crate reads. Deliberately a closed enum
/// (not a raw string) so a typo'd field name is a compile error, not a
/// silent no-op lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldName {
    PhoneNumber,
    PhoneNumberPrefix,
    Email,
    AddressStreet1,
    AddressStreet2,
    AddressCity,
    AddressState,
    AddressPostalCode,
    AltContactFirstName,
    AltContactLastName,
    AltContactEmail,
    AltContactPhoneNumber,
    AltContactPhoneNumberPrefix,
    AltContactAddressStreet1,
    AltContactAddressStreet2,
    AltContactAddressCity,
    AltContactAddressState,
    AltContactAddressPostalCode,
    CompanyName,
    FirstName,
    LastName,
}

pub struct FieldSpec {
    pub name: FieldName,
    pub category: FieldCategory,
    pub kind: FieldKind,
}

/// One row per column the comparison pass walks, in the reference
/// script's own declaration order. `kind: Address` marks every field
/// (including alternate-contact address sub-fields) that goes through
/// street-suffix/period normalization rather than plain case/whitespace
/// normalization — mirrors the union of `FIELD_CATEGORIES["address"]`
/// and the alt-contact address fields in `ADDRESS_FIELDS`.
pub const FIELD_SPECS: &[FieldSpec] = &[
    FieldSpec { name: FieldName::PhoneNumber, category: FieldCategory::Phone, kind: FieldKind::Plain },
    FieldSpec { name: FieldName::PhoneNumberPrefix, category: FieldCategory::Phone, kind: FieldKind::Plain },
    FieldSpec { name: FieldName::Email, category: FieldCategory::Email, kind: FieldKind::Plain },
    FieldSpec { name: FieldName::AddressStreet1, category: FieldCategory::Address, kind: FieldKind::Address },
    FieldSpec { name: FieldName::AddressStreet2, category: FieldCategory::Address, kind: FieldKind::Address },
    FieldSpec { name: FieldName::AddressCity, category: FieldCategory::Address, kind: FieldKind::Address },
    FieldSpec { name: FieldName::AddressState, category: FieldCategory::Address, kind: FieldKind::Address },
    FieldSpec { name: FieldName::AddressPostalCode, category: FieldCategory::Address, kind: FieldKind::Address },
    FieldSpec { name: FieldName::AltContactFirstName, category: FieldCategory::AltContact, kind: FieldKind::Plain },
    FieldSpec { name: FieldName::AltContactLastName, category: FieldCategory::AltContact, kind: FieldKind::Plain },
    FieldSpec { name: FieldName::AltContactEmail, category: FieldCategory::AltContact, kind: FieldKind::Plain },
    FieldSpec { name: FieldName::AltContactPhoneNumber, category: FieldCategory::AltContact, kind: FieldKind::Plain },
    FieldSpec { name: FieldName::AltContactPhoneNumberPrefix, category: FieldCategory::AltContact, kind: FieldKind::Plain },
    FieldSpec { name: FieldName::AltContactAddressStreet1, category: FieldCategory::AltContact, kind: FieldKind::Address },
    FieldSpec { name: FieldName::AltContactAddressStreet2, category: FieldCategory::AltContact, kind: FieldKind::Address },
    FieldSpec { name: FieldName::AltContactAddressCity, category: FieldCategory::AltContact, kind: FieldKind::Address },
    FieldSpec { name: FieldName::AltContactAddressState, category: FieldCategory::AltContact, kind: FieldKind::Address },
    FieldSpec { name: FieldName::AltContactAddressPostalCode, category: FieldCategory::AltContact, kind: FieldKind::Address },
    FieldSpec { name: FieldName::CompanyName, category: FieldCategory::Company, kind: FieldKind::Plain },
    FieldSpec { name: FieldName::FirstName, category: FieldCategory::Name, kind: FieldKind::Plain },
    FieldSpec { name: FieldName::LastName, category: FieldCategory::Name, kind: FieldKind::Plain },
];

/// One tenant/unit row from a QMS End Users export. Only the columns
/// this crate's logic actually reads — the full 71-column passthrough
/// needed for the eventual export artifact is an API/export-layer
/// concern, not this crate's (see project memory: output format is an
/// unresolved product decision).
#[derive(Debug, Clone, Default)]
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

    /// `display_name` from the reference script: "FirstName LastName",
    /// falling back to `FirtLast` when both name parts are blank.
    pub fn display_name(&self) -> String {
        let name = format!("{} {}", self.first_name.trim(), self.last_name.trim());
        let name = name.trim();
        if name.is_empty() {
            self.first_last.trim().to_uppercase()
        } else {
            name.to_uppercase()
        }
    }
}

/// A category that differed within a group, and which specific fields
/// under it differed (for the detail lines under a flagged group).
#[derive(Debug, Clone)]
pub struct FieldMismatch {
    pub category: FieldCategory,
    pub fields: Vec<FieldName>,
}

/// A tenant's records grouped by exact `FirtLast` match (pass 1). Only
/// groups with 2+ records are ever produced downstream.
#[derive(Debug, Clone)]
pub struct TenantGroup {
    pub key: String,
    pub records: Vec<TenantRecord>,
}

/// A multi-unit group with at least one contact-info disagreement —
/// the output of pass 2/3 (comparison + note assignment).
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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

