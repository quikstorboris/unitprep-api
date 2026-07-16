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

use crate::analysis::parse_fingerprint;
use unitprep_core::csv_document::CsvDocument;
use crate::models::Severity;

pub use issues::{
    correctable_fields_for,
    is_dimension_exemptable,
    ValidationIssue,
};

struct ColumnIndices {
    unit_group: usize,
    number: usize,
    width: Option<usize>,
    length: Option<usize>,
    locality: Option<usize>,
    climate_controlled: Option<usize>,
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

    let mut blank = Vec::new();
    let mut odd = Vec::new();
    let mut bad_dimensions = Vec::new();
    let mut climate_mismatches =
        Vec::new();
    let mut locality_mismatches =
        Vec::new();
    let mut unitgroup_dimension_mismatches =
        Vec::new();

    let mut unit_counts: HashMap<
        String,
        usize,
    > = HashMap::new();

    let mut group_counts: HashMap<
        String,
        usize,
    > = HashMap::new();

    let mut casing_map: HashMap<
        String,
        Vec<String>,
    > = HashMap::new();

    for row in &document.rows {
        let unit = row
            .get(indices.number)
            .cloned()
            .unwrap_or_default();

        let group = row
            .get(indices.unit_group)
            .map(|v| v.trim())
            .unwrap_or("");

        if !group.is_empty() {
            *group_counts
                .entry(group.to_string())
                .or_insert(0) += 1;
        }

        if !unit.is_empty() {
            *unit_counts
                .entry(unit.clone())
                .or_insert(0) += 1;

            casing_map
                .entry(unit.to_lowercase())
                .or_default()
                .push(unit.clone());
        }

        match row_checks::classify_group_value(
            group,
        ) {
            row_checks::GroupValue::Ok => {}

            row_checks::GroupValue::Blank => {
                blank.push(unit.clone());
            }

            row_checks::GroupValue::Suspicious => {
                odd.push(unit.clone());
            }
        }

        if !dimension_exempt_units
            .contains(&unit)
            && row_checks::has_bad_dimensions(
                row,
                indices.width,
                indices.length,
            )
        {
            bad_dimensions
                .push(unit.clone());
        }

        let fingerprint =
            parse_fingerprint(group);

        if row_checks::climate_mismatches_group(
            row,
            indices.climate_controlled,
            &fingerprint,
        ) {
            climate_mismatches
                .push(unit.clone());
        }

        if row_checks::locality_mismatches_group(
            row,
            indices.locality,
            &fingerprint,
        ) {
            locality_mismatches
                .push(unit.clone());
        }

        if row_checks::dimensions_mismatch_group(
            row,
            indices.width,
            indices.length,
            &fingerprint,
        ) {
            unitgroup_dimension_mismatches
                .push(unit.clone());
        }
    }

    let rare_and_single_unit_groups =
        group_checks::single_occurrence_groups(
            &group_counts,
        );

    let casing_issues =
        group_checks::casing_inconsistencies(
            casing_map,
        );

    let duplicate_units =
        group_checks::duplicate_units(
            unit_counts,
        );

    // (flagged values, description, severity) — severity lives right
    // next to the description it belongs to, so the two can never drift
    // apart the way they could when severity was reconstructed
    // elsewhere by matching against this same description text.
    Ok(issues::build([
        (
            blank,
            issues::BLANK_UNITGROUP,
            Severity::Error,
        ),
        (
            odd,
            issues::SUSPICIOUS_UNITGROUP,
            Severity::Warning,
        ),
        (
            duplicate_units,
            issues::DUPLICATE_UNITS,
            Severity::Error,
        ),
        (
            bad_dimensions,
            issues::INVALID_DIMENSIONS,
            Severity::Error,
        ),
        (
            climate_mismatches,
            issues::CLIMATE_MISMATCH,
            Severity::Error,
        ),
        (
            locality_mismatches,
            issues::LOCALITY_MISMATCH,
            Severity::Error,
        ),
        (
            unitgroup_dimension_mismatches,
            issues::UNITGROUP_DIMENSION_MISMATCH,
            Severity::Error,
        ),
        (
            rare_and_single_unit_groups
                .clone(),
            issues::RARE_GROUP,
            Severity::Warning,
        ),
        (
            rare_and_single_unit_groups,
            issues::SINGLE_UNIT_GROUP,
            Severity::Warning,
        ),
        (
            casing_issues,
            issues::INCONSISTENT_CASING,
            Severity::Warning,
        ),
    ]))
}


#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;
