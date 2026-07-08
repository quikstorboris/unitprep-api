// Pure, per-row validation checks. Each function inspects one row (plus
// whatever column indices/context it needs) and returns whether that
// check found a problem — no shared state, no I/O. That makes each one
// independently unit-testable with a two-cell row instead of a whole
// CsvDocument, and keeps `validate_document`'s single pass over rows
// (see mod.rs) a simple sequence of calls rather than a wall of inline
// logic.

use crate::domain::analysis::{
    Climate,
    GroupFingerprint,
    Location,
};

/// How the UnitGroup value on a row reads at a glance.
pub(super) enum GroupValue {
    Ok,
    Blank,
    /// Contains a comma — usually a sign two group names got merged.
    Suspicious,
}

pub(super) fn classify_group_value(
    group: &str,
) -> GroupValue {
    if group.is_empty() {
        GroupValue::Blank
    } else if group.contains(',') {
        GroupValue::Suspicious
    } else {
        GroupValue::Ok
    }
}

fn parses_as_positive(
    row: &[String],
    idx: usize,
) -> bool {
    row.get(idx)
        .map(|v| v.trim())
        .and_then(|v| v.parse::<f64>().ok())
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
        .any(|idx| {
            !parses_as_positive(row, idx)
        })
}

/// True if a declared "climate controlled" yes/no column disagrees with
/// the Climate/Non-Climate implied by the UnitGroup name itself.
pub(super) fn climate_mismatches_group(
    row: &[String],
    climate_controlled_idx: Option<usize>,
    fingerprint: &GroupFingerprint,
) -> bool {
    let Some(idx) = climate_controlled_idx
    else {
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
        (Some(expected), Some(declared)) => {
            expected != declared
        }
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
        "outside" => {
            Some(Location::Outside)
        }
        _ => None,
    };

    match (fingerprint.location, declared) {
        (Some(expected), Some(declared)) => {
            expected != declared
        }
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
    let (Some(width_idx), Some(length_idx)) =
        (width_idx, length_idx)
    else {
        return false;
    };

    let actual_width = row
        .get(width_idx)
        .map(|v| v.trim());

    let actual_length = row
        .get(length_idx)
        .map(|v| v.trim());

    match (
        fingerprint.width.as_deref(),
        fingerprint.length.as_deref(),
        actual_width,
        actual_length,
    ) {
        (
            Some(fp_width),
            Some(fp_length),
            Some(actual_width),
            Some(actual_length),
        ) => {
            fp_width != actual_width
                || fp_length
                    != actual_length
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::analysis::parse_fingerprint;

    fn row(values: &[&str]) -> Vec<String> {
        values
            .iter()
            .map(|v| v.to_string())
            .collect()
    }

    #[test]
    fn classifies_blank_and_suspicious_and_ok_group_values() {
        assert!(matches!(
            classify_group_value(""),
            GroupValue::Blank
        ));

        assert!(matches!(
            classify_group_value(
                "10x10, 10x20"
            ),
            GroupValue::Suspicious
        ));

        assert!(matches!(
            classify_group_value(
                "10x10 Inside Climate"
            ),
            GroupValue::Ok
        ));
    }

    #[test]
    fn bad_dimensions_flags_non_positive_values_only_for_present_columns() {
        let good = row(&["10", "20"]);

        assert!(!has_bad_dimensions(
            &good,
            Some(0),
            Some(1)
        ));

        let zero_width =
            row(&["0", "20"]);

        assert!(has_bad_dimensions(
            &zero_width,
            Some(0),
            Some(1)
        ));

        // No dimension columns in this file at all — nothing to flag.
        assert!(!has_bad_dimensions(
            &good, None, None
        ));
    }

    #[test]
    fn climate_mismatch_detects_disagreement_with_group_name() {
        let fingerprint = parse_fingerprint(
            "10x10 Inside Climate",
        );

        let declared_no =
            row(&["A01", "No"]);

        assert!(climate_mismatches_group(
            &declared_no,
            Some(1),
            &fingerprint
        ));

        let declared_yes =
            row(&["A01", "Yes"]);

        assert!(!climate_mismatches_group(
            &declared_yes,
            Some(1),
            &fingerprint
        ));

        assert!(!climate_mismatches_group(
            &declared_no,
            None,
            &fingerprint
        ));
    }

    #[test]
    fn locality_mismatch_detects_disagreement_with_group_name() {
        let fingerprint = parse_fingerprint(
            "10x10 Outside Non-Climate",
        );

        let declared_inside =
            row(&["A01", "Inside"]);

        assert!(locality_mismatches_group(
            &declared_inside,
            Some(1),
            &fingerprint
        ));

        let declared_outside =
            row(&["A01", "Outside"]);

        assert!(
            !locality_mismatches_group(
                &declared_outside,
                Some(1),
                &fingerprint
            )
        );
    }

    #[test]
    fn dimensions_mismatch_detects_disagreement_with_group_name() {
        let fingerprint = parse_fingerprint(
            "10x20 Inside Climate",
        );

        let wrong_length =
            row(&["A01", "10", "15"]);

        assert!(
            dimensions_mismatch_group(
                &wrong_length,
                Some(1),
                Some(2),
                &fingerprint
            )
        );

        let correct =
            row(&["A01", "10", "20"]);

        assert!(
            !dimensions_mismatch_group(
                &correct,
                Some(1),
                Some(2),
                &fingerprint
            )
        );
    }
}
