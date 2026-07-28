//! Value normalization for comparison. Ported from the reference
//! script's `norm_value` / `is_empty` / `STREET_SUFFIXES` — including
//! the 2026-07-14 fix that strips periods *before* other punctuation so
//! `"P.O. Box"` and `"PO Box"` both collapse to `"po box"`.

use crate::types::FieldKind;

/// Street-suffix and direction abbreviations, so e.g. "Avenue" and "Ave"
/// compare equal after normalization. Direct port of the reference
/// script's `STREET_SUFFIXES` table.
pub const STREET_SUFFIXES: &[(&str, &str)] = &[
    ("street", "st"),
    ("avenue", "ave"),
    ("av", "ave"),
    ("boulevard", "blvd"),
    ("drive", "dr"),
    ("lane", "ln"),
    ("road", "rd"),
    ("court", "ct"),
    ("circle", "cir"),
    ("place", "pl"),
    ("terrace", "ter"),
    ("highway", "hwy"),
    ("parkway", "pkwy"),
    ("trail", "trl"),
    ("way", "way"),
    ("north", "n"),
    ("south", "s"),
    ("east", "e"),
    ("west", "w"),
    ("northeast", "ne"),
    ("northwest", "nw"),
    ("southeast", "se"),
    ("southwest", "sw"),
    ("apartment", "apt"),
    ("suite", "ste"),
    ("unit", "unit"),
];

/// True for blank, whitespace-only, or absent values.
pub fn is_empty(value: &str) -> bool {
    value.trim().is_empty()
}

/// Case-insensitive normalization; `FieldKind::Address` values are
/// further normalized (period-stripped, punctuation-stripped, each
/// token run through the street-suffix table); `FieldKind::Phone`
/// values are reduced to digits only.
pub fn normalize_value(kind: FieldKind, value: &str) -> String {
    if is_empty(value) {
        return String::new();
    }
    let v = value.trim().to_lowercase();
    match kind {
        FieldKind::Address => normalize_address(&v),
        FieldKind::Phone => normalize_phone(&v),
        FieldKind::Plain => collapse_whitespace(&v),
    }
}

pub(crate) fn collapse_whitespace(v: &str) -> String {
    v.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Reduces a phone number to its digits only, so formatting differences
/// alone ("(831) 555-1234" vs. "831-555-1234" vs. "8315551234") never
/// register as either a real mismatch (comparison.rs) or a missed
/// shared-value relationship (relatedness.rs) — both previously
/// compared these as plain case/whitespace-normalized strings, meaning
/// only an exact-formatting match would agree.
fn normalize_phone(v: &str) -> String {
    v.chars().filter(|c| c.is_ascii_digit()).collect()
}

fn normalize_address(v: &str) -> String {
    // `v` is already trim+lowercased by the caller. Periods are dropped
    // first (so "p.o." collapses to "po", matching "PO") before other
    // punctuation is replaced with a space — reversing this order was
    // the exact bug the 2026-07-14 script revision fixed.
    let without_periods: String = v.chars().filter(|&c| c != '.').collect();
    let punctuation_as_space: String = without_periods
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect();
    punctuation_as_space
        .split_whitespace()
        .map(suffix_or_self)
        .collect::<Vec<_>>()
        .join(" ")
}

fn suffix_or_self(token: &str) -> &str {
    STREET_SUFFIXES
        .iter()
        .find(|(long, _)| *long == token)
        .map(|(_, short)| *short)
        .unwrap_or(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected values computed by actually running the reference
    /// script's `norm_value` logic in Python on these inputs.
    fn assert_address(value: &str, expected: &str) {
        assert_eq!(normalize_value(FieldKind::Address, value), expected);
    }

    #[test]
    fn period_stripped_before_other_punctuation() {
        assert_address("P.O. Box 123", "po box 123");
        assert_address("PO Box 123", "po box 123");
    }

    #[test]
    fn street_suffix_and_abbreviation_forms_match() {
        assert_address("123 Main Street", "123 main st");
        assert_address("123 Main St.", "123 main st");
    }

    #[test]
    fn direction_abbreviation_and_period_both_normalize() {
        assert_address("400 S. Dupont hwy", "400 s dupont hwy");
        assert_address("550 South DuPont Pkwy", "550 s dupont pkwy");
    }

    #[test]
    fn blank_and_whitespace_only_are_empty() {
        assert_eq!(normalize_value(FieldKind::Address, ""), "");
        assert_eq!(normalize_value(FieldKind::Address, "   "), "");
    }

    #[test]
    fn phone_formatting_differences_normalize_equal() {
        assert_eq!(
            normalize_value(FieldKind::Phone, "(831) 555-1234"),
            normalize_value(FieldKind::Phone, "831-555-1234")
        );
        assert_eq!(
            normalize_value(FieldKind::Phone, "831.555.1234"),
            normalize_value(FieldKind::Phone, "8315551234")
        );
        assert_eq!(
            normalize_value(FieldKind::Phone, "(831) 555-1234"),
            "8315551234"
        );
    }

    #[test]
    fn phone_normalization_still_treats_blank_as_empty() {
        assert_eq!(normalize_value(FieldKind::Phone, ""), "");
        assert_eq!(normalize_value(FieldKind::Phone, "   "), "");
    }

    #[test]
    fn genuinely_different_phone_numbers_stay_different() {
        assert_ne!(
            normalize_value(FieldKind::Phone, "(831) 555-1234"),
            normalize_value(FieldKind::Phone, "(831) 555-9999")
        );
    }

    /// Names (`FirstName`/`LastName`, etc.) are `FieldKind::Plain` --
    /// case/whitespace-normalized only, same as the reference script's
    /// `norm_value`, which relies on Python's plain `str.lower()` and
    /// does nothing further to a name. Neither strips diacritics, so
    /// "José" and "Jose" normalize to two visibly different strings and
    /// compare as different names rather than being folded together as
    /// the same tenant. This is a coverage/regression test locking in
    /// that inherited, documented behavior -- not a claim that it's the
    /// ideal behavior for a future revision to keep.
    #[test]
    fn diacritics_in_names_are_preserved_not_folded_to_ascii() {
        assert_eq!(normalize_value(FieldKind::Plain, "José"), "josé");
        assert_eq!(normalize_value(FieldKind::Plain, "Jose"), "jose");
        assert_ne!(
            normalize_value(FieldKind::Plain, "José"),
            normalize_value(FieldKind::Plain, "Jose")
        );
    }
}
