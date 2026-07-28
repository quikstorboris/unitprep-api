//! Standalone name-quality heuristics — informational review hints
//! surfaced to the user before export, never used by the matching/
//! analysis logic in `fingerprint::mod`, which has its own, much
//! stricter equality rules.

use once_cell::sync::Lazy;
use regex::Regex;

use super::{parse_fingerprint, DIMENSION_REGEX};

/// A rough, deliberately permissive heuristic for "this doesn't look like
/// a real storage-unit group name" — no parseable width/length at all
/// (the common case: apartment-style, office-space, or bare-square-
/// footage listings that occasionally end up in a unit list), or a
/// degenerate 0x0 dimension. Purely a review hint surfaced to the user
/// before export — never used for the matching/analysis logic above,
/// which already has its own, much stricter equality rules.
pub fn is_uncommon_group_name(name: &str) -> bool {
    let fingerprint = parse_fingerprint(name);

    match (&fingerprint.width, &fingerprint.length) {
        (Some(w), Some(l)) => is_zero(w) || is_zero(l),
        _ => true,
    }
}

fn is_zero(value: &str) -> bool {
    value.parse::<f64>().map(|v| v == 0.0).unwrap_or(false)
}

/// A name where the *whole* trimmed string is a single number ("10"), or
/// an "x"/"X" between two single letters ("aXb") -- deliberately narrow
/// (anchored to the entire string) so it never fires on ordinary English
/// words that merely happen to contain the letter x ("Extra", "Flex
/// Space", "Boxed Storage"). A wider "digits x digits" attempt with the
/// second number missing ("10x") is covered by `DIMENSION_REGEX` itself
/// failing to match while still looking like an attempt -- see
/// `has_malformed_dimension_attempt`.
static MALFORMED_DIMENSION_REGEX: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^\d+(?:\.\d+)?\s*x\s*\d*(?:\.\d+)?$|^[a-z]\s*x\s*[a-z]$").unwrap()
});

/// True if `name` looks like it's *trying* to express a WxL dimension —
/// a bare number ("10"), a digit-x-digit pattern missing its second
/// number ("10x"), or letters standing in for numbers ("aXb") — but
/// doesn't actually parse as two positive numbers. Distinct from
/// `is_uncommon_group_name`: that flags names with *no* dimension
/// attempt at all (pure descriptive text like "Hertz Office Space", or a
/// single letter) as well as a genuinely zero-valued dimension ("0x0"),
/// which already parses cleanly as two numbers and isn't "malformed" in
/// this sense. The two checks are deliberately kept mutually exclusive
/// (a name is never both) so validation can flag it as one or the
/// other, never both at once.
pub fn has_malformed_dimension_attempt(name: &str) -> bool {
    let trimmed = name.trim();

    if trimmed.parse::<f64>().is_ok() {
        return true;
    }

    if DIMENSION_REGEX.is_match(trimmed) {
        // Already a clean, fully-formed dimension (including the
        // degenerate 0x0 case, which `is_uncommon_group_name` already
        // accounts for on its own) -- nothing malformed about the
        // pattern itself.
        return false;
    }

    MALFORMED_DIMENSION_REGEX.is_match(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_with_no_parseable_dimension_are_uncommon() {
        for name in [
            "1 bd, 1 ba",
            "165 sq ft",
            "2 bd, 1 ba",
            "99 sq ft",
            "Hertz Office Space",
            "sq ft",
        ] {
            assert!(
                is_uncommon_group_name(name),
                "expected {name:?} to be flagged uncommon"
            );
        }
    }

    #[test]
    fn a_zero_by_zero_dimension_is_uncommon() {
        assert!(is_uncommon_group_name("0X0 OFFICE SPACE CLIMATE"));
    }

    #[test]
    fn a_real_dimension_is_not_uncommon() {
        assert!(!is_uncommon_group_name("10x10 Climate Controlled"));
    }

    #[test]
    fn flags_a_bare_number_a_dangling_x_and_letters_for_numbers_as_malformed() {
        for name in ["10", "10x", "aXb"] {
            assert!(
                has_malformed_dimension_attempt(name),
                "expected {name:?} to be flagged malformed"
            );
        }
    }

    #[test]
    fn single_letters_are_not_malformed() {
        assert!(!has_malformed_dimension_attempt("A"));
    }

    #[test]
    fn ordinary_words_containing_the_letter_x_are_not_malformed() {
        for name in ["Extra Storage", "Flex Space", "Boxed Storage"] {
            assert!(
                !has_malformed_dimension_attempt(name),
                "expected {name:?} to NOT be flagged malformed"
            );
        }
    }

    #[test]
    fn a_clean_dimension_including_the_degenerate_zero_case_is_not_malformed() {
        assert!(!has_malformed_dimension_attempt("10x10 Inside Climate"));

        assert!(!has_malformed_dimension_attempt("0X0 OFFICE SPACE CLIMATE"));
    }

    #[test]
    fn every_currently_confirmed_uncommon_name_stays_unmalformed() {
        // These are the exact 7 names discovery's own review list
        // surfaces on the real Wave 3 dataset -- confirming this new
        // check never steals any of them away from `is_uncommon_group_name`
        // and double-counts them under a different reason.
        for name in [
            "0X0 OFFICE SPACE CLIMATE",
            "1 bd, 1 ba",
            "165 sq ft",
            "2 bd, 1 ba",
            "99 sq ft",
            "Hertz Office Space",
            "sq ft",
        ] {
            assert!(
                !has_malformed_dimension_attempt(name),
                "expected {name:?} to NOT be flagged malformed"
            );
        }
    }
}
