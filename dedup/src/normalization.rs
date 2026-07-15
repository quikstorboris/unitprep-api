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
/// token run through the street-suffix table).
pub fn normalize_value(kind: FieldKind, value: &str) -> String {
    if is_empty(value) {
        return String::new();
    }
    let v = value.trim().to_lowercase();
    match kind {
        FieldKind::Address => normalize_address(&v),
        FieldKind::Plain => collapse_whitespace(&v),
    }
}

fn collapse_whitespace(v: &str) -> String {
    v.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalize_address(v: &str) -> String {
    // `v` is already trim+lowercased by the caller. Periods are dropped
    // first (so "p.o." collapses to "po", matching "PO") before other
    // punctuation is replaced with a space — reversing this order was
    // the exact bug the 2026-07-14 script revision fixed.
    let without_periods: String = v.chars().filter(|&c| c != '.').collect();
    let punctuation_as_space: String = without_periods
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c.is_whitespace() { c } else { ' ' })
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
}
