use std::collections::HashSet;

use crate::models::Severity;

// Description strings are named constants — not literals duplicated between
// the check that raises an issue (mod.rs) and `correctable_fields_for` below
// — so a typo in one copy can't silently desync a check from its
// correctable-fields entry.
pub const BLANK_UNITGROUP: &str = "Blank UnitGroup values";
pub const ODD_UNITGROUP: &str = "Odd UnitGroup values";
pub const DUPLICATE_UNITS: &str = "Duplicate unit numbers";
pub const INVALID_DIMENSIONS: &str = "Invalid dimensions";
pub const CLIMATE_MISMATCH: &str = "Climate status does not match UnitGroup";
pub const LOCALITY_MISMATCH: &str = "Locality does not match UnitGroup";
pub const UNITGROUP_DIMENSION_MISMATCH: &str =
    "UnitGroup dimensions do not match Width/Length";
pub const RARE_GROUP: &str = "Rare UnitGroup detected";
pub const INCONSISTENT_CASING: &str = "Inconsistent unit-number casing";

/// Group names a user has explicitly accepted "as is" for one of the two
/// per-group checks (Odd/Rare) -- distinct from `excluded_groups`
/// (`Session`/`corrections.rs`): an acknowledged group's units stay in
/// the data exactly as uploaded (nothing removed, nothing renamed), just
/// stops being flagged under *that one check*. A group both odd and rare
/// needs its own acknowledgment per check -- accepting it as "odd" (a
/// legitimately non-standard name) says nothing about whether its low
/// occurrence count is also fine.
#[derive(Debug, Clone, Default)]
pub struct GroupCheckAcknowledgments {
    pub odd: HashSet<String>,
    pub rare: HashSet<String>,
}

/// Which columns a given issue description can be fixed by editing a single
/// value in, for the inline-correction feature — empty means review-only
/// (duplicate unit numbers and casing conflicts need a "which one wins"
/// decision, not a value swap, so they're deliberately not listed here).
/// Area is never listed: it's derived from Width × Length, not an
/// independent value a user should be asked to type in.
pub fn correctable_fields_for(
    description: &str,
) -> Vec<String> {
    let fields: &[&str] = match description {
        INVALID_DIMENSIONS => {
            &["width", "length"]
        }

        CLIMATE_MISMATCH => {
            &["climatecontrolled"]
        }

        LOCALITY_MISMATCH => &["locality"],

        UNITGROUP_DIMENSION_MISMATCH => {
            &["width", "length"]
        }

        // Not `ODD_UNITGROUP` -- that's a per-group check now (see
        // `flagged_are_group_names`), fixed via `/correct-group`, not a
        // single-unit `/correct` field swap.
        BLANK_UNITGROUP => {
            &["unitgroup"]
        }

        _ => &[],
    };

    fields
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// True only for "Invalid dimensions" — the one check where a unit can
/// legitimately have no dimensions at all (an office, an owner's
/// apartment, etc. in the catalog), so the right fix is exempting the
/// unit from the check rather than fabricating a Width/Length value.
pub fn is_dimension_exemptable(
    description: &str,
) -> bool {
    description == INVALID_DIMENSIONS
}

/// A single validation finding for one file.
///
/// Severity is assigned by the caller at the point each check is
/// created — deliberately not left for anyone downstream to infer later
/// (e.g. by matching on `description` text). A description is free-form
/// English for humans; it should never double as a machine-readable
/// classification key.
#[derive(Debug, Clone)]
pub struct ValidationIssue {
    /// Unit numbers for per-unit checks; group names for the two
    /// per-group checks (rare/odd groups) — see
    /// `flagged_are_group_names`, which says which.
    pub flagged_values: Vec<String>,
    pub description: String,
    pub severity: Severity,

    /// True only for the two per-group checks (rare/odd groups), where
    /// `flagged_values` holds group names directly rather than unit
    /// numbers — lets a caller (see `api::validate::run_validation`)
    /// resolve "which UnitGroup(s) does this issue concern" without
    /// guessing from `description` text, same reasoning as why severity
    /// is assigned here instead of inferred downstream.
    pub flagged_are_group_names: bool,

    /// (group name, occurrence count) pairs -- populated only for
    /// `RARE_GROUP`, where the actual count (anywhere from 1 up to the
    /// rare-group threshold) is meaningful to show next to each name.
    /// Empty for every other check. Kept separate from `flagged_values`
    /// rather than encoded into the string itself, since that string is
    /// also the literal group name `/correct-group` matches units
    /// against -- baking "(3)" into it would break that lookup.
    pub group_occurrence_counts: Vec<(String, usize)>,
}

/// Turns a fixed list of (flagged values, description, severity, flagged
/// values are group names) candidates into the issues that actually have
/// something to report — i.e. drops any candidate whose list came back
/// empty. `group_occurrence_counts` defaults empty; set it on the
/// returned `RARE_GROUP` issue afterward if needed (see mod.rs).
pub(super) fn build<const N: usize>(
    candidates: [(
        Vec<String>,
        &str,
        Severity,
        bool,
    ); N],
) -> Vec<ValidationIssue> {
    candidates
        .into_iter()
        .filter(|(flagged_values, _, _, _)| {
            !flagged_values.is_empty()
        })
        .map(
            |(
                flagged_values,
                description,
                severity,
                flagged_are_group_names,
            )| {
                ValidationIssue {
                    flagged_values,
                    description:
                        description
                            .to_string(),
                    severity,
                    flagged_are_group_names,
                    group_occurrence_counts:
                        Vec::new(),
                }
            },
        )
        .collect()
}
