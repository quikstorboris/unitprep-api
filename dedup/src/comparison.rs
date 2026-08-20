//! Pass 2: within a multi-unit group, find which contact-info
//! categories disagree. Ported from the reference script's
//! `find_differing_categories` / `contact_info_matches`. Blank vs.
//! filled counts as differing (an incomplete record is a mismatch, not
//! a match) — except that a `FieldKind::Plain` value which is itself a
//! placeholder (`"None"`, `"N/A"`, ...) counts as blank too, not as
//! real differing text. See `is_blank_for_comparison`.

use crate::normalization::{is_empty, is_placeholder, normalize_value};
use crate::types::{
    FieldKind, FieldMismatch, FieldName, FieldValueMismatch, TenantRecord, CATEGORY_PRIORITY,
    FIELD_SPECS,
};

/// Whether `raw` should be treated as blank for mismatch-detection
/// purposes — genuinely blank, or (for `FieldKind::Plain` fields only) a
/// placeholder like `"None"`/`"N/A"` typed as a stand-in for leaving the
/// field empty. Real bug this closes: `"None"` sitting in
/// `AlternateContactLastName` compared as real, differing text against
/// a genuinely blank cell on another unit, flagging a spurious AltContact
/// mismatch.
///
/// Deliberately scoped to `Plain` only — `Phone`/`Address` do NOT get
/// this treatment here, unlike in `relatedness.rs`, which excludes
/// placeholders from every signal regardless of kind. A phone field
/// containing `"N/A"` is still a real, correctable mismatch against a
/// genuinely blank phone field under this crate's own "blank vs. filled
/// always differs" rule (see `field_matches_across`'s doc comment and
/// its `a_garbage_phone_value_does_not_match_a_genuinely_blank_one`
/// regression test) — that data-quality signal is exactly what a
/// facility manager needs to see for a field like Phone/Address, where
/// "something was typed, even if it's garbage" is meaningfully
/// different information from "nothing was typed at all." A name-like
/// free-text field (AlternateContactLastName, CompanyName, ...) has no
/// such distinction to lose: "None" and blank both unambiguously mean
/// "no value," so treating them as equal loses nothing.
fn is_blank_for_comparison(kind: FieldKind, raw: &str) -> bool {
    is_empty(raw) || (kind == FieldKind::Plain && is_placeholder(raw))
}

/// For each field category (in priority order), checks whether any of
/// its fields differ (after normalization) across `group`. Returns one
/// `FieldMismatch` per category that has at least one differing field,
/// in priority order — mirrors the reference script's `differing` /
/// `differing_fields`. Blank vs. filled counts as differing (an
/// incomplete record is a mismatch, not a match), same as the
/// reference script's plain set-based comparison.
pub fn find_differing_categories(group: &[TenantRecord]) -> Vec<FieldMismatch> {
    let mut result = Vec::new();
    for category in CATEGORY_PRIORITY {
        let differing_fields: Vec<FieldValueMismatch> = FIELD_SPECS
            .iter()
            .filter(|spec| spec.category == category)
            .filter(|spec| !field_matches_across(group, spec.name, spec.kind))
            .map(|spec| FieldValueMismatch {
                field: spec.name,
                values: distinct_display_values(group, spec.name, spec.kind),
            })
            .collect();
        if !differing_fields.is_empty() {
            result.push(FieldMismatch {
                category,
                fields: differing_fields,
            });
        }
    }
    result
}

/// Distinct raw (trimmed, not normalized) values for `field` across
/// `group`, blank shown as `"(blank)"`, sorted with blank last — same
/// display convention as the reference script's console summary
/// (`sorted({...}, key=lambda x: (x == "(blank)", x))`).
///
/// Dedupes on the same `(is_blank, normalized value)` key
/// `field_matches_across` uses to decide a mismatch, not the raw string —
/// otherwise two values that count as "the same" there (two emails
/// differing only in case, two phone numbers differing only in
/// formatting) would still show up as two separate "distinct values"
/// here, overstating how much the group's data actually disagrees.
fn distinct_display_values(
    group: &[TenantRecord],
    field: FieldName,
    kind: crate::types::FieldKind,
) -> Vec<String> {
    let mut by_key: std::collections::BTreeMap<(bool, String), String> =
        std::collections::BTreeMap::new();

    for record in group {
        let raw = record.field(field).trim();
        let (key, display) = blank_aware_key(kind, raw);
        by_key.entry(key).or_insert(display);
    }

    let mut values: Vec<String> = by_key.into_values().collect();
    values.sort_by_key(|v| blank_last_sort_key(v));
    values
}

/// The `(is_blank, normalized value)` key both `distinct_display_values`
/// above and `phrasing::units_by_value` group records by, paired with the
/// display string for a raw value (`"(blank)"` for a blank value, the raw
/// value otherwise). Shared so both call sites treat "the same value" the
/// exact same way `field_matches_across` below does when it decides a
/// mismatch -- not a merely similar rule kept in sync by hand.
pub(crate) fn blank_aware_key(
    kind: crate::types::FieldKind,
    raw: &str,
) -> ((bool, String), String) {
    let display = if is_blank_for_comparison(kind, raw) {
        "(blank)".to_string()
    } else {
        raw.to_string()
    };
    (comparison_key(kind, raw), display)
}

/// The `(is_blank-for-comparison, normalized-or-empty value)` pair both
/// `blank_aware_key` and `field_matches_across` compare on. A value that
/// counts as blank for comparison purposes (genuinely blank, or a
/// `FieldKind::Plain` placeholder — see `is_blank_for_comparison`)
/// always contributes an empty normalized value here, not whatever
/// `normalize_value` would otherwise produce (`"none"` for the literal
/// string `"None"`) — otherwise a placeholder and a genuinely blank
/// value would carry the same `blank` flag but still compare unequal on
/// the second half of the tuple, silently undoing the whole point of
/// treating them as the same value.
fn comparison_key(kind: crate::types::FieldKind, raw: &str) -> (bool, String) {
    let blank = is_blank_for_comparison(kind, raw);
    let normalized = if blank {
        String::new()
    } else {
        normalize_value(kind, raw)
    };
    (blank, normalized)
}

/// Sort key that puts `"(blank)"` last, then alphabetically -- the shared
/// display convention both `distinct_display_values` and
/// `phrasing::units_by_value` use for cross-record value listings.
pub(crate) fn blank_last_sort_key(value: &str) -> (bool, String) {
    (value == "(blank)", value.to_string())
}

/// True if every non-`Name`-category field already matches (after
/// normalization) across `group`. Used by the typo-variant pass to
/// decide the confirmation note's wording, not whether to surface a
/// candidate (see crate-level docs: always flag).
pub fn contact_info_matches(group: &[TenantRecord]) -> bool {
    FIELD_SPECS
        .iter()
        .filter(|spec| !matches!(spec.category, crate::types::FieldCategory::Name))
        .all(|spec| field_matches_across(group, spec.name, spec.kind))
}

/// A value that's genuinely blank and one that merely *normalizes* to an
/// empty string (e.g. a phone field containing "N/A" or "----", which
/// `normalize_phone` reduces to "" for having no digits) must never
/// compare equal here -- per this crate's own rule that blank-vs-filled
/// always counts as a mismatch, not a match, UNLESS `comparison_key`'s
/// own placeholder carve-out for `FieldKind::Plain` applies (a name-like
/// field's `"None"`/`"N/A"` is nothing worth distinguishing from blank —
/// see `is_blank_for_comparison`). Comparing raw blank-ness alongside
/// the normalized value (rather than the normalized value alone) is
/// what keeps the general rule honest for fields where normalization
/// can produce an empty string from non-empty input.
fn field_matches_across(
    group: &[TenantRecord],
    name: FieldName,
    kind: crate::types::FieldKind,
) -> bool {
    let mut values = group
        .iter()
        .map(|r| comparison_key(kind, r.field(name)));
    let first = match values.next() {
        Some(v) => v,
        None => return true,
    };
    values.all(|v| v == first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FieldCategory;

    fn record(email: &str) -> TenantRecord {
        TenantRecord {
            email: email.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn differing_field_carries_actual_distinct_values() {
        let group = vec![record("a@example.com"), record("")];
        let differing = find_differing_categories(&group);

        let email_mismatch = differing
            .iter()
            .find(|m| m.category == FieldCategory::Email)
            .expect("email should be flagged as differing");

        assert_eq!(email_mismatch.fields.len(), 1);
        assert_eq!(email_mismatch.fields[0].field, FieldName::Email);
        // Blank sorts last, matching the reference script's display convention.
        assert_eq!(
            email_mismatch.fields[0].values,
            vec!["a@example.com", "(blank)"]
        );
    }

    #[test]
    fn matching_field_is_not_reported_as_differing() {
        let group = vec![record("same@example.com"), record("same@example.com")];
        let differing = find_differing_categories(&group);
        assert!(differing.iter().all(|m| m.category != FieldCategory::Email));
    }

    #[test]
    fn differently_formatted_phone_numbers_are_not_reported_as_differing() {
        let group = vec![
            TenantRecord {
                phone_number: "(555) 123-4567".to_string(),
                ..Default::default()
            },
            TenantRecord {
                phone_number: "555-123-4567".to_string(),
                ..Default::default()
            },
        ];
        let differing = find_differing_categories(&group);
        assert!(differing.iter().all(|m| m.category != FieldCategory::Phone));
    }

    /// Regression test: a phone value that normalizes to "" for having no
    /// digits at all ("N/A") must not silently match a genuinely blank
    /// phone value just because both normalize the same way.
    #[test]
    fn a_garbage_phone_value_does_not_match_a_genuinely_blank_one() {
        let group = vec![
            TenantRecord {
                phone_number: "N/A".to_string(),
                ..Default::default()
            },
            TenantRecord {
                phone_number: "".to_string(),
                ..Default::default()
            },
        ];
        let differing = find_differing_categories(&group);
        assert!(
            differing.iter().any(|m| m.category == FieldCategory::Phone),
            "a garbage phone value and a blank one should still count as a mismatch"
        );
    }

    /// Regression test: two emails differing only in case must collapse
    /// to a single displayed value, not two -- they already count as the
    /// same value per `field_matches_across`'s normalized comparison, so
    /// the displayed "distinct values" list must agree.
    #[test]
    fn case_variant_values_are_not_shown_as_two_distinct_values() {
        let group = vec![record("Bob@Test.com"), record("bob@test.com"), record("")];
        let differing = find_differing_categories(&group);

        let email_mismatch = differing
            .iter()
            .find(|m| m.category == FieldCategory::Email)
            .expect("email should be flagged as differing (blank vs. filled)");

        assert_eq!(
            email_mismatch.fields[0].values.len(),
            2,
            "the two case-variant emails should collapse into one displayed value, \
             leaving just that value plus \"(blank)\": {:?}",
            email_mismatch.fields[0].values
        );
    }

    /// `PhoneNumberPrefix`/`AlternateContactPhoneNumberPrefix` differences
    /// — even fully populated, genuinely different values, not just
    /// blank-vs-filled — must never surface a Phone mismatch. Per the
    /// reference skill's own current rationale, legacy QSX never exposed
    /// this field to users, so any difference here is migration noise,
    /// not a correctable discrepancy; unlike every other field, this one
    /// isn't in `FIELD_SPECS` at all (see `FieldName`'s doc comment).
    #[test]
    fn differing_phone_prefixes_never_flag_a_mismatch() {
        let group = vec![
            TenantRecord {
                phone_number: "5551234567".to_string(),
                phone_number_prefix: "1".to_string(),
                ..Default::default()
            },
            TenantRecord {
                phone_number: "5551234567".to_string(),
                phone_number_prefix: "2".to_string(),
                ..Default::default()
            },
        ];
        let differing = find_differing_categories(&group);
        assert!(
            differing.iter().all(|m| m.category != FieldCategory::Phone),
            "a phone prefix difference alone should never be a Phone mismatch: {differing:?}"
        );
    }

    /// Two genuinely blank phone values must still match each other (not
    /// regress into every pair of blanks becoming a false mismatch).
    #[test]
    fn two_genuinely_blank_phone_values_still_match() {
        let group = vec![
            TenantRecord {
                phone_number: "".to_string(),
                ..Default::default()
            },
            TenantRecord {
                phone_number: "   ".to_string(),
                ..Default::default()
            },
        ];
        let differing = find_differing_categories(&group);
        assert!(differing.iter().all(|m| m.category != FieldCategory::Phone));
    }

    /// Regression test for a real bug (Westpark facility, unit 257):
    /// the literal string `"None"` in `AlternateContactLastName` on one
    /// unit, genuinely blank on another, must not surface a spurious
    /// AltContact mismatch — `"None"` here unambiguously means "no
    /// alternate contact," the same thing a blank cell means.
    #[test]
    fn a_none_placeholder_in_a_plain_field_matches_a_genuinely_blank_one() {
        let group = vec![
            TenantRecord {
                alt_contact_last_name: "None".to_string(),
                ..Default::default()
            },
            TenantRecord {
                alt_contact_last_name: "".to_string(),
                ..Default::default()
            },
        ];
        let differing = find_differing_categories(&group);
        assert!(
            differing.iter().all(|m| m.category != FieldCategory::AltContact),
            "a placeholder value in a Plain field must match a genuinely blank one: {differing:?}"
        );
    }

    /// The same placeholder carve-out must NOT extend to Phone/Address —
    /// unlike a Plain field, "something was typed, even garbage" is real
    /// signal there (see `is_blank_for_comparison`'s own doc comment).
    /// This is the existing `a_garbage_phone_value_does_not_match_a_
    /// genuinely_blank_one` test's own guarantee, re-asserted here using
    /// a literal placeholder token ("N/A") specifically, since that's
    /// the exact overlap case this fix could have silently broken.
    #[test]
    fn a_placeholder_token_as_a_phone_value_still_differs_from_blank() {
        let group = vec![
            TenantRecord {
                phone_number: "N/A".to_string(),
                ..Default::default()
            },
            TenantRecord {
                phone_number: "".to_string(),
                ..Default::default()
            },
        ];
        let differing = find_differing_categories(&group);
        assert!(
            differing.iter().any(|m| m.category == FieldCategory::Phone),
            "a placeholder token in a Phone field must still count as a mismatch against blank"
        );
    }
}
