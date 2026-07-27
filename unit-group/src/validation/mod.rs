// Validates a unit file's rows once, checking structural sanity (blank
// UnitGroup, malformed dimensions), cross-referencing declared columns
// (width/length/locality/climate) against what the UnitGroup name itself
// implies, and flagging aggregate oddities (duplicate units, rare groups,
// inconsistent casing). Area is intentionally not validated — it's a
// derived value (Width × Length), not an independent fact.
//
// The row-level checks live in row_checks.rs, the aggregate ones in
// group_checks.rs, and the ValidationIssue type + builder in issues.rs —
// this file only owns the single pass over `document.rows` and wiring
// each check's result into the right accumulator. Keeping the checks as
// small named functions rather than inline blocks is what makes it
// practical to unit-test each one in isolation (see the tests in
// row_checks.rs/group_checks.rs) instead of only being able to exercise
// them through a full CsvDocument here.

mod group_checks;
mod issues;
mod row_checks;

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use crate::analysis::{
    has_malformed_dimension_attempt,
    parse_fingerprint,
};
use unitprep_core::csv_document::CsvDocument;
use crate::models::Severity;

pub use issues::{
    correctable_fields_for,
    is_dimension_exemptable,
    GroupCheckAcknowledgments,
    ValidationIssue,
    ODD_UNITGROUP,
    RARE_GROUP,
};

struct ColumnIndices {
    unit_group: usize,
    number: usize,
    width: Option<usize>,
    length: Option<usize>,
    locality: Option<usize>,
    climate_controlled: Option<usize>,
}

/// Everything the single pass over `document.rows` accumulates, bundled
/// into one struct rather than eight independent locals threaded through
/// the loop below — `record_row` reads as "update the scan" instead of
/// eight separately-synced mutations that all have to stay in step by
/// convention alone.
#[derive(Default)]
struct RowScan {
    blank: Vec<String>,
    bad_dimensions: Vec<String>,
    climate_mismatches: Vec<String>,
    locality_mismatches: Vec<String>,
    unitgroup_dimension_mismatches: Vec<String>,
    unit_counts: HashMap<String, usize>,
    group_counts: HashMap<String, usize>,
    casing_map: HashMap<String, Vec<String>>,
}

impl RowScan {
    fn record_row(
        &mut self,
        row: &[String],
        indices: &ColumnIndices,
        dimension_exempt_units: &HashSet<String>,
    ) {
        let unit = row
            .get(indices.number)
            .cloned()
            .unwrap_or_default();

        let group = row
            .get(indices.unit_group)
            .map(|v| v.trim())
            .unwrap_or("");

        if !group.is_empty() {
            *self
                .group_counts
                .entry(group.to_string())
                .or_insert(0) += 1;
        }

        if !unit.is_empty() {
            *self
                .unit_counts
                .entry(unit.clone())
                .or_insert(0) += 1;

            self.casing_map
                .entry(unit.to_lowercase())
                .or_default()
                .push(unit.clone());
        }

        match row_checks::classify_group_value(
            group,
        ) {
            row_checks::GroupValue::Ok => {}

            row_checks::GroupValue::Blank => {
                self.blank.push(unit.clone());
            }
        }

        // A malformed dimension *attempt* in the group name itself
        // ("10x", "aXb", a bare "10") is always Invalid, regardless of
        // what the row's own Width/Length columns say -- the name is
        // the problem. A group with no dimension attempt at all (an
        // office, a single letter, "sq ft") is Odd instead (see
        // `odd_group_names` below) and is deliberately *not* also
        // flagged here just because its Width/Length columns are
        // blank -- that's expected for a non-dimensioned group, not a
        // data-quality problem. Only a group whose name looks like a
        // real, valid dimension falls through to the original check:
        // do the row's *own* declared Width/Length columns actually
        // agree there's a real, positive value there.
        let group_is_malformed =
            has_malformed_dimension_attempt(group);

        // `is_odd_group_name` already excludes malformed attempts on
        // its own (see its doc comment) -- no need to repeat that
        // exclusion here.
        if group_is_malformed
            || (!group_checks::is_odd_group_name(group)
                && !dimension_exempt_units
                    .contains(&unit)
                && row_checks::has_bad_dimensions(
                    row,
                    indices.width,
                    indices.length,
                ))
        {
            self.bad_dimensions
                .push(unit.clone());
        }

        let fingerprint =
            parse_fingerprint(group);

        if row_checks::climate_mismatches_group(
            row,
            indices.climate_controlled,
            &fingerprint,
        ) {
            self.climate_mismatches
                .push(unit.clone());
        }

        if row_checks::locality_mismatches_group(
            row,
            indices.locality,
            &fingerprint,
        ) {
            self.locality_mismatches
                .push(unit.clone());
        }

        if row_checks::dimensions_mismatch_group(
            row,
            indices.width,
            indices.length,
            &fingerprint,
        ) {
            self.unitgroup_dimension_mismatches
                .push(unit.clone());
        }
    }
}

impl ColumnIndices {
    /// `None` if the two columns every other check depends on
    /// (UnitGroup, Number) aren't present — the rest are optional,
    /// since not every unit file carries dimension/locality/climate
    /// columns to cross-check against. Area is deliberately not tracked
    /// here — it's a derived value (Width × Length), not something
    /// validated or corrected independently.
    fn discover(
        document: &CsvDocument,
    ) -> Option<Self> {
        Some(Self {
            unit_group: document
                .header_index("unitgroup")?,
            number: document
                .header_index("number")?,
            width: document
                .header_index("width"),
            length: document
                .header_index("length"),
            locality: document
                .header_index("locality"),
            climate_controlled: document
                .header_index(
                    "climatecontrolled",
                ),
        })
    }
}

pub fn validate_document(
    document: &CsvDocument,
    dimension_exempt_units: &HashSet<String>,
    group_check_acknowledgments: &GroupCheckAcknowledgments,
) -> Result<Vec<ValidationIssue>> {
    // Discovery already classified this file as a unit file (that's the
    // only way it reaches `validate_document` at all — see
    // `api::discover::is_unit_document`), which means it already found
    // UnitGroup/Number/Category headers. If `ColumnIndices::discover`
    // still can't find them here, that's not "nothing to validate" —
    // it's an internal inconsistency between discovery's and
    // validation's column lookup that must never be silently swallowed
    // as a clean zero-issues result. Fail loudly instead: the caller
    // (see `api::validate`) already treats an `Err` here as "skip this
    // file and log a warning," rather than counting it as checked.
    let Some(indices) =
        ColumnIndices::discover(document)
    else {
        anyhow::bail!(
            "'{}' was classified as a unit file but its required UnitGroup/Number columns could not be found — this indicates a bug in column discovery, not a clean file",
            document.file_name
        );
    };

    let mut scan = RowScan::default();

    for row in &document.rows {
        scan.record_row(
            row,
            &indices,
            dimension_exempt_units,
        );
    }

    // A handful of units of the same type is exactly the situation
    // where a typo or a wrong dimension can hide undetected -- Boris's
    // call, replacing the earlier "exactly one" threshold. A group the
    // user has explicitly accepted "as is" (see
    // `GroupCheckAcknowledgments`) is filtered out here, before
    // `issues::build` ever sees it -- its units stay in the data
    // unchanged, they just stop being flagged under this one check.
    let rare_pairs: Vec<(String, usize)> =
        group_checks::rare_groups(
            &scan.group_counts,
            RARE_GROUP_MAX_OCCURRENCES,
        )
        .into_iter()
        .filter(|(name, _)| {
            !group_check_acknowledgments
                .rare
                .contains(name)
        })
        .collect();

    let rare_group_names: Vec<String> =
        rare_pairs
            .iter()
            .map(|(name, _)| name.clone())
            .collect();

    let odd_group_names: Vec<String> =
        group_checks::odd_group_names(
            &scan.group_counts,
        )
        .into_iter()
        .filter(|name| {
            !group_check_acknowledgments
                .odd
                .contains(name)
        })
        .collect();

    let casing_issues =
        group_checks::casing_inconsistencies(
            scan.casing_map,
        );

    let duplicate_units =
        group_checks::duplicate_units(
            scan.unit_counts,
        );

    // (flagged values, description, severity, flagged values are group
    // names) — severity lives right next to the description it belongs
    // to, so the two can never drift apart the way they could when
    // severity was reconstructed elsewhere by matching against this
    // same description text. Only the two per-group checks (rare/odd
    // groups) set the last field true — every other check's flagged
    // values are unit numbers.
    //
    // Dimension/climate/locality mismatches and invalid dimensions are
    // Warning, not Error: a UnitGroup that doesn't fit the standard
    // width×length storage-unit shape (an office, a boat slip, an
    // apartment) legitimately has nothing to cross-check against, so
    // these can't reliably distinguish "real data problem" from
    // "non-standard but correct" — blocking export on them would punish
    // every odd-but-valid group along with genuine typos.
    //
    // Odd and Invalid Dimensions are deliberately mutually exclusive: a
    // group with no dimension attempt at all (an office, "sq ft", a
    // single letter) is Odd only, even though its actual Width/Length
    // columns are typically also blank -- that's expected for a
    // non-dimensioned group, not a second, separate data-quality
    // problem. A group whose name *attempts* a dimension but botches it
    // ("10x", "aXb", a bare "10") is Invalid Dimensions only, regardless
    // of its actual columns.
    let mut issues = issues::build([
        (
            scan.blank,
            issues::BLANK_UNITGROUP,
            Severity::Error,
            false,
        ),
        (
            odd_group_names,
            issues::ODD_UNITGROUP,
            Severity::Warning,
            true,
        ),
        (
            duplicate_units,
            issues::DUPLICATE_UNITS,
            Severity::Error,
            false,
        ),
        (
            scan.bad_dimensions,
            issues::INVALID_DIMENSIONS,
            Severity::Warning,
            false,
        ),
        (
            scan.climate_mismatches,
            issues::CLIMATE_MISMATCH,
            Severity::Warning,
            false,
        ),
        (
            scan.locality_mismatches,
            issues::LOCALITY_MISMATCH,
            Severity::Warning,
            false,
        ),
        (
            scan.unitgroup_dimension_mismatches,
            issues::UNITGROUP_DIMENSION_MISMATCH,
            Severity::Warning,
            false,
        ),
        (
            rare_group_names,
            issues::RARE_GROUP,
            Severity::Warning,
            true,
        ),
        (
            casing_issues,
            issues::INCONSISTENT_CASING,
            Severity::Warning,
            false,
        ),
    ]);

    // `rare_pairs` carries each flagged group's actual occurrence count
    // (1 up to `RARE_GROUP_MAX_OCCURRENCES`) -- attached after `build`
    // rather than threading a 5th tuple field through every candidate
    // above, since every other check leaves this empty.
    for issue in &mut issues {
        if issue.description == issues::RARE_GROUP {
            issue.group_occurrence_counts =
                rare_pairs.clone();
        }
    }

    Ok(issues)
}

/// A UnitGroup appearing on this many units or fewer in a file counts as
/// "rare" -- a handful of units of one type is exactly the situation
/// where a typo or a wrong dimension could be hiding undetected. Chosen
/// deliberately wider than "exactly one" (the previous, since-removed
/// "UnitGroup contains only one unit" check's own threshold) after real
/// test data showed genuinely unremarkable small groups being flagged
/// at that narrower cutoff.
const RARE_GROUP_MAX_OCCURRENCES: usize = 4;

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;
