//! The field taxonomy: every QMS column this crate reads, which
//! contact-info category it belongs to, and how its value should be
//! normalized. Mirrors the reference script's `FIELD_CATEGORIES` (as of
//! the 2026-07-14 revision: `CompanyName` has its own category, split
//! out from `name`).

use serde::Serialize;

/// A contact-info category a tenant group can disagree on. Declared in
/// the exact priority order the reference script uses to pick which
/// note to show when multiple categories differ at once (first match
/// in this order wins).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
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
/// (street-suffix/direction lookup, period-stripping), phone-specific
/// normalization (strip everything but digits, so formatting
/// differences like "(831) 555-1234" vs. "8315551234" compare equal),
/// or just case/whitespace normalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Plain,
    Address,
    Phone,
}

/// Every QMS export column this crate reads. Deliberately a closed enum
/// (not a raw string) so a typo'd field name is a compile error, not a
/// silent no-op lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
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
    FieldSpec {
        name: FieldName::PhoneNumber,
        category: FieldCategory::Phone,
        kind: FieldKind::Phone,
    },
    FieldSpec {
        name: FieldName::PhoneNumberPrefix,
        category: FieldCategory::Phone,
        kind: FieldKind::Phone,
    },
    FieldSpec {
        name: FieldName::Email,
        category: FieldCategory::Email,
        kind: FieldKind::Plain,
    },
    FieldSpec {
        name: FieldName::AddressStreet1,
        category: FieldCategory::Address,
        kind: FieldKind::Address,
    },
    FieldSpec {
        name: FieldName::AddressStreet2,
        category: FieldCategory::Address,
        kind: FieldKind::Address,
    },
    FieldSpec {
        name: FieldName::AddressCity,
        category: FieldCategory::Address,
        kind: FieldKind::Address,
    },
    FieldSpec {
        name: FieldName::AddressState,
        category: FieldCategory::Address,
        kind: FieldKind::Address,
    },
    FieldSpec {
        name: FieldName::AddressPostalCode,
        category: FieldCategory::Address,
        kind: FieldKind::Address,
    },
    FieldSpec {
        name: FieldName::AltContactFirstName,
        category: FieldCategory::AltContact,
        kind: FieldKind::Plain,
    },
    FieldSpec {
        name: FieldName::AltContactLastName,
        category: FieldCategory::AltContact,
        kind: FieldKind::Plain,
    },
    FieldSpec {
        name: FieldName::AltContactEmail,
        category: FieldCategory::AltContact,
        kind: FieldKind::Plain,
    },
    FieldSpec {
        name: FieldName::AltContactPhoneNumber,
        category: FieldCategory::AltContact,
        kind: FieldKind::Phone,
    },
    FieldSpec {
        name: FieldName::AltContactPhoneNumberPrefix,
        category: FieldCategory::AltContact,
        kind: FieldKind::Phone,
    },
    FieldSpec {
        name: FieldName::AltContactAddressStreet1,
        category: FieldCategory::AltContact,
        kind: FieldKind::Address,
    },
    FieldSpec {
        name: FieldName::AltContactAddressStreet2,
        category: FieldCategory::AltContact,
        kind: FieldKind::Address,
    },
    FieldSpec {
        name: FieldName::AltContactAddressCity,
        category: FieldCategory::AltContact,
        kind: FieldKind::Address,
    },
    FieldSpec {
        name: FieldName::AltContactAddressState,
        category: FieldCategory::AltContact,
        kind: FieldKind::Address,
    },
    FieldSpec {
        name: FieldName::AltContactAddressPostalCode,
        category: FieldCategory::AltContact,
        kind: FieldKind::Address,
    },
    FieldSpec {
        name: FieldName::CompanyName,
        category: FieldCategory::Company,
        kind: FieldKind::Plain,
    },
    FieldSpec {
        name: FieldName::FirstName,
        category: FieldCategory::Name,
        kind: FieldKind::Plain,
    },
    FieldSpec {
        name: FieldName::LastName,
        category: FieldCategory::Name,
        kind: FieldKind::Plain,
    },
];

/// Looks up `name`'s declared `FieldKind` — for the few call sites (e.g.
/// `phrasing::units_by_value`) that only have a bare `FieldName` on hand,
/// not the full `FieldSpec` a `FIELD_SPECS` iteration already carries.
pub fn kind_for(name: FieldName) -> FieldKind {
    FIELD_SPECS
        .iter()
        .find(|spec| spec.name == name)
        .map(|spec| spec.kind)
        .expect("every FieldName has a FIELD_SPECS entry")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exhaustive match with no wildcard arm — adding a new `FieldName`
    /// variant without updating both this match and the `all` list below
    /// is a compile error, not a silent gap `kind_for`'s `.expect(...)`
    /// would only discover at runtime.
    fn all_field_names() -> Vec<FieldName> {
        fn exhaustiveness_check(name: FieldName) {
            match name {
                FieldName::PhoneNumber
                | FieldName::PhoneNumberPrefix
                | FieldName::Email
                | FieldName::AddressStreet1
                | FieldName::AddressStreet2
                | FieldName::AddressCity
                | FieldName::AddressState
                | FieldName::AddressPostalCode
                | FieldName::AltContactFirstName
                | FieldName::AltContactLastName
                | FieldName::AltContactEmail
                | FieldName::AltContactPhoneNumber
                | FieldName::AltContactPhoneNumberPrefix
                | FieldName::AltContactAddressStreet1
                | FieldName::AltContactAddressStreet2
                | FieldName::AltContactAddressCity
                | FieldName::AltContactAddressState
                | FieldName::AltContactAddressPostalCode
                | FieldName::CompanyName
                | FieldName::FirstName
                | FieldName::LastName => {}
            }
        }

        let all = vec![
            FieldName::PhoneNumber,
            FieldName::PhoneNumberPrefix,
            FieldName::Email,
            FieldName::AddressStreet1,
            FieldName::AddressStreet2,
            FieldName::AddressCity,
            FieldName::AddressState,
            FieldName::AddressPostalCode,
            FieldName::AltContactFirstName,
            FieldName::AltContactLastName,
            FieldName::AltContactEmail,
            FieldName::AltContactPhoneNumber,
            FieldName::AltContactPhoneNumberPrefix,
            FieldName::AltContactAddressStreet1,
            FieldName::AltContactAddressStreet2,
            FieldName::AltContactAddressCity,
            FieldName::AltContactAddressState,
            FieldName::AltContactAddressPostalCode,
            FieldName::CompanyName,
            FieldName::FirstName,
            FieldName::LastName,
        ];
        for name in &all {
            exhaustiveness_check(*name);
        }
        all
    }

    /// Exhaustive match with no wildcard arm — same guarantee as
    /// `all_field_names` above, but for `FieldCategory`.
    fn all_field_categories() -> Vec<FieldCategory> {
        fn exhaustiveness_check(category: FieldCategory) {
            match category {
                FieldCategory::Phone
                | FieldCategory::Email
                | FieldCategory::Address
                | FieldCategory::AltContact
                | FieldCategory::Company
                | FieldCategory::Name => {}
            }
        }

        let all = vec![
            FieldCategory::Phone,
            FieldCategory::Email,
            FieldCategory::Address,
            FieldCategory::AltContact,
            FieldCategory::Company,
            FieldCategory::Name,
        ];
        for category in &all {
            exhaustiveness_check(*category);
        }
        all
    }

    #[test]
    fn field_specs_has_exactly_one_entry_per_field_name_variant() {
        for name in all_field_names() {
            let count = FIELD_SPECS.iter().filter(|spec| spec.name == name).count();
            assert_eq!(
                count, 1,
                "{name:?} should have exactly one FIELD_SPECS entry, found {count}"
            );
        }

        assert_eq!(
            FIELD_SPECS.len(),
            all_field_names().len(),
            "FIELD_SPECS should have no rows beyond one per FieldName variant"
        );
    }

    #[test]
    fn category_priority_has_exactly_one_entry_per_field_category_variant() {
        for category in all_field_categories() {
            let count = CATEGORY_PRIORITY.iter().filter(|&&c| c == category).count();
            assert_eq!(
                count, 1,
                "{category:?} should appear exactly once in CATEGORY_PRIORITY, found {count}"
            );
        }

        assert_eq!(
            CATEGORY_PRIORITY.len(),
            all_field_categories().len(),
            "CATEGORY_PRIORITY should have no entries beyond one per FieldCategory variant"
        );
    }
}
