//! The field taxonomy: every QMS column this crate reads, which
//! contact-info category it belongs to, and how its value should be
//! normalized. Mirrors the reference script's `FIELD_CATEGORIES` (as of
//! the 2026-07-14 revision: `CompanyName` has its own category, split
//! out from `name`).

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
