//! Two small, self-contained pieces of `run_validation`'s per-document
//! loop, pulled out so that loop reads as "for each issue, resolve it
//! and summarize it" instead of burying both a HashMap-building block
//! and a ~50-line struct-construction block inline between the
//! severity-counting and the `issues.push(...)` call.

use std::collections::HashMap;

use unitprep_core::csv_document::CsvDocument;
use unitprep_unit_group::{
    correctable_fields_for, is_dimension_exemptable, ValidationIssue, ValidationIssueSummary,
};

/// Cheap per-document lookup, built once regardless of how many issues
/// reference it -- lets a per-unit issue's flagged unit numbers be
/// resolved back to the UnitGroup they belong to (see
/// `issue_to_summary`'s `affected_group_names`), without
/// `validate_document` itself needing to carry that mapping through its
/// own return type.
pub(super) fn build_unit_to_group_map(document: &CsvDocument) -> HashMap<String, String> {
    match (
        document.header_index("number"),
        document.header_index("unitgroup"),
    ) {
        (Some(unit_idx), Some(group_idx)) => document
            .rows
            .iter()
            .filter_map(|row| {
                // Trimmed to match how the unit number is read
                // everywhere else it's used as an identifier (see
                // validation's `record_row`) -- otherwise a stray
                // leading/trailing space here would fail to resolve a
                // flagged unit back to its UnitGroup.
                let unit = row.get(unit_idx)?.trim().to_string();
                let group = row.get(group_idx)?.trim().to_string();

                if unit.is_empty() {
                    None
                } else {
                    Some((unit, group))
                }
            })
            .collect(),
        _ => HashMap::new(),
    }
}

/// Turns one `unitprep_unit_group` validation finding into this API's
/// own `ValidationIssueSummary` shape. Severity counting stays in
/// `run_validation` itself (it accumulates across every issue in the
/// loop, not something this per-issue conversion owns).
pub(super) fn issue_to_summary(
    file_name: &str,
    issue: ValidationIssue,
    unit_to_group: &HashMap<String, String>,
) -> ValidationIssueSummary {
    let mut affected_group_names: Vec<String> = if issue.flagged_are_group_names {
        issue.flagged_values.clone()
    } else {
        issue
            .flagged_values
            .iter()
            .filter_map(|unit| unit_to_group.get(unit).cloned())
            .filter(|group| !group.is_empty())
            .collect()
    };

    affected_group_names.sort();
    affected_group_names.dedup();

    let flagged_are_group_names = issue.flagged_are_group_names;

    let group_occurrence_counts = issue.group_occurrence_counts.clone();

    let affected_unit_ids = issue.flagged_values;

    let detail = format!(
        "{} unit{}: {}",
        affected_unit_ids.len(),
        if affected_unit_ids.len() == 1 {
            ""
        } else {
            "s"
        },
        affected_unit_ids.join(", "),
    );

    let correctable_fields = correctable_fields_for(&issue.description);

    let exemptable = is_dimension_exemptable(&issue.description);

    ValidationIssueSummary {
        file_name: file_name.to_string(),
        severity: issue.severity,
        description: issue.description,
        affected_units: affected_unit_ids.len(),
        affected_unit_ids,
        detail,
        correctable_fields,
        exemptable,
        affected_group_names,
        flagged_are_group_names,
        group_occurrence_counts,
    }
}
