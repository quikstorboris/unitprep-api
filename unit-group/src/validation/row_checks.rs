// Pure, per-row validation checks. Each function inspects one row (plus
// whatever column indices/context it needs) and returns whether that
// check found a problem — no shared state, no I/O. That makes each one
// independently unit-testable with a two-cell row instead of a whole
// CsvDocument, and keeps `validate_document`'s single pass over rows
// (see mod.rs) a simple sequence of calls rather than a wall of inline
// logic.

use crate::analysis::{Climate, GroupFingerprint, Location};

/// How the UnitGroup value on a row reads at a glance. "Odd" values
/// (comma-merged, or no parseable dimension) are a group-level property,
/// not a per-row one -- see `group_checks::odd_group_names`, which
/// checks each distinct group name once rather than every row that
/// happens to carry it.
pub(super) enum GroupValue {
    Ok,
    Blank,
}

pub(super) fn classify_group_value(group: &str) -> GroupValue {
    if group.is_empty() {
        GroupValue::Blank
    } else {
        GroupValue::Ok
    }
}

/// Parses a dimension value as `f64`, accepting a comma decimal separator
/// ("10,5") as a fallback when the plain period-decimal parse fails --
/// only for the narrow, unambiguous shape of digits-comma-digits with no
/// period already present, so a genuine thousands-separated value (which
/// this app never expects for a unit width/length) is never misread.
/// Without this, a comma-decimal locale's otherwise-valid positive number
/// was rejected outright as an "Invalid dimensions" false positive.
fn parse_dimension_number(value: &str) -> Option<f64> {
    if let Ok(parsed) = value.parse::<f64>() {
        return Some(parsed);
    }

    let mut parts = value.splitn(2, ',');
    let (whole, fraction) = (parts.next()?, parts.next()?);

    if fraction.contains(',') || whole.is_empty() || fraction.is_empty() {
        return None;
    }

    if !whole.chars().all(|c| c.is_ascii_digit() || c == '-')
        || !fraction.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }

    format!("{whole}.{fraction}").parse::<f64>().ok()
}

fn parses_as_positive(row: &[String], idx: usize) -> bool {
    row.get(idx)
        .map(|v| v.trim())
        .and_then(parse_dimension_number)
        .is_some_and(|v| v > 0.0)
}

/// True if any *present* width/length column fails to parse as a
/// positive number. Columns that don't exist in this file are not
/// checked — this only flags data that's actually there and wrong.
/// Area is deliberately not considered here: it's a derived value
/// (Width × Length), not an independent fact a facility export needs to
/// carry or a user should ever be asked to type in directly.
pub(super) fn has_bad_dimensions(
    row: &[String],
    width_idx: Option<usize>,
    length_idx: Option<usize>,
) -> bool {
    [width_idx, length_idx]
        .into_iter()
        .flatten()
        .any(|idx| !parses_as_positive(row, idx))
}

/// True if a declared "climate controlled" yes/no column disagrees with
/// the Climate/Non-Climate implied by the UnitGroup name itself.
pub(super) fn climate_mismatches_group(
    row: &[String],
    climate_controlled_idx: Option<usize>,
    fingerprint: &GroupFingerprint,
) -> bool {
    let Some(idx) = climate_controlled_idx else {
        return false;
    };

    let value = row
        .get(idx)
        .map(|v| v.trim().to_lowercase())
        .unwrap_or_default();

    let declared = match value.as_str() {
        "yes" => Some(Climate::Climate),
        "no" => Some(Climate::NonClimate),
        _ => None,
    };

    match (fingerprint.climate, declared) {
        (Some(expected), Some(declared)) => expected != declared,
        _ => false,
    }
}

/// True if a declared Inside/Outside locality column disagrees with the
/// location implied by the UnitGroup name itself.
pub(super) fn locality_mismatches_group(
    row: &[String],
    locality_idx: Option<usize>,
    fingerprint: &GroupFingerprint,
) -> bool {
    let Some(idx) = locality_idx else {
        return false;
    };

    let value = row
        .get(idx)
        .map(|v| v.trim().to_lowercase())
        .unwrap_or_default();

    let declared = match value.as_str() {
        "inside" => Some(Location::Inside),
        "outside" => Some(Location::Outside),
        _ => None,
    };

    match (fingerprint.location, declared) {
        (Some(expected), Some(declared)) => expected != declared,
        _ => false,
    }
}

/// True if the declared width/length columns disagree with the
/// dimensions implied by the UnitGroup name itself (e.g. a "10x20"
/// group with Width=10, Length=15 in the data).
pub(super) fn dimensions_mismatch_group(
    row: &[String],
    width_idx: Option<usize>,
    length_idx: Option<usize>,
    fingerprint: &GroupFingerprint,
) -> bool {
    let (Some(width_idx), Some(length_idx)) = (width_idx, length_idx) else {
        return false;
    };

    let actual_width = row.get(width_idx).map(|v| v.trim());

    let actual_length = row.get(length_idx).map(|v| v.trim());

    match (
        fingerprint.width.as_deref(),
        fingerprint.length.as_deref(),
        actual_width,
        actual_length,
    ) {
        (Some(fp_width), Some(fp_length), Some(actual_width), Some(actual_length)) => {
            dimension_values_differ(fp_width, actual_width)
                || dimension_values_differ(fp_length, actual_length)
        }
        _ => false,
    }
}

/// Compares two dimension strings numerically when both parse as a
/// number (so "10" and "10.0" agree, instead of the raw string equality
/// this function used before), falling back to a literal string
/// comparison only when either side isn't a plain number.
fn dimension_values_differ(a: &str, b: &str) -> bool {
    match (parse_dimension_number(a), parse_dimension_number(b)) {
        (Some(a), Some(b)) => a != b,
        _ => a != b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::parse_fingerprint;

    fn row(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn classifies_blank_and_ok_group_values() {
        assert!(matches!(classify_group_value(""), GroupValue::Blank));

        assert!(matches!(
            classify_group_value("10x10 Inside Climate"),
            GroupValue::Ok
        ));
    }

    #[test]
    fn bad_dimensions_flags_non_positive_values_only_for_present_columns() {
        let good = row(&["10", "20"]);

        assert!(!has_bad_dimensions(&good, Some(0), Some(1)));

        let zero_width = row(&["0", "20"]);

        assert!(has_bad_dimensions(&zero_width, Some(0), Some(1)));

        // No dimension columns in this file at all — nothing to flag.
        assert!(!has_bad_dimensions(&good, None, None));
    }

    /// Regression test: a comma-decimal value ("10,5") is a legitimately
    /// formatted positive number in a comma-decimal locale and must not
    /// be flagged as an invalid dimension just because `str::parse::<f64>`
    /// alone rejects the comma.
    #[test]
    fn comma_decimal_dimension_values_are_accepted_as_positive() {
        let row = row(&["10,5", "20"]);

        assert!(!has_bad_dimensions(&row, Some(0), Some(1)));
    }

    #[test]
    fn comma_decimal_dimension_value_agrees_with_its_period_equivalent() {
        assert!(!dimension_values_differ("10,5", "10.5"));
    }

    /// A bare digit-comma-digit shape is the only comma pattern accepted
    /// -- anything else (a second comma, no digits on one side) still
    /// fails to parse rather than being guessed at.
    #[test]
    fn malformed_comma_values_still_fail_to_parse_as_positive() {
        let row = row(&["10,5,2", "20"]);

        assert!(has_bad_dimensions(&row, Some(0), Some(1)));
    }

    #[test]
    fn climate_mismatch_detects_disagreement_with_group_name() {
        let fingerprint = parse_fingerprint("10x10 Inside Climate");

        let declared_no = row(&["A01", "No"]);

        assert!(climate_mismatches_group(
            &declared_no,
            Some(1),
            &fingerprint
        ));

        let declared_yes = row(&["A01", "Yes"]);

        assert!(!climate_mismatches_group(
            &declared_yes,
            Some(1),
            &fingerprint
        ));

        assert!(!climate_mismatches_group(&declared_no, None, &fingerprint));
    }

    #[test]
    fn locality_mismatch_detects_disagreement_with_group_name() {
        let fingerprint = parse_fingerprint("10x10 Outside Non-Climate");

        let declared_inside = row(&["A01", "Inside"]);

        assert!(locality_mismatches_group(
            &declared_inside,
            Some(1),
            &fingerprint
        ));

        let declared_outside = row(&["A01", "Outside"]);

        assert!(!locality_mismatches_group(
            &declared_outside,
            Some(1),
            &fingerprint
        ));
    }

    #[test]
    fn dimensions_mismatch_detects_disagreement_with_group_name() {
        let fingerprint = parse_fingerprint("10x20 Inside Climate");

        let wrong_length = row(&["A01", "10", "15"]);

        assert!(dimensions_mismatch_group(
            &wrong_length,
            Some(1),
            Some(2),
            &fingerprint
        ));

        let correct = row(&["A01", "10", "20"]);

        assert!(!dimensions_mismatch_group(
            &correct,
            Some(1),
            Some(2),
            &fingerprint
        ));
    }

    #[test]
    fn dimensions_mismatch_ignores_pure_formatting_differences() {
        let fingerprint = parse_fingerprint("10x20 Inside Climate");

        // "10.0"/"20.0" are numerically identical to the fingerprint's
        // "10"/"20" — this must not be flagged as a real mismatch.
        let differently_formatted = row(&["A01", "10.0", "20.0"]);

        assert!(!dimensions_mismatch_group(
            &differently_formatted,
            Some(1),
            Some(2),
            &fingerprint
        ));

        // A genuinely different value must still be flagged even when
        // both sides parse as numbers.
        let really_wrong = row(&["A01", "10.5", "20"]);

        assert!(dimensions_mismatch_group(
            &really_wrong,
            Some(1),
            Some(2),
            &fingerprint
        ));
    }
}
