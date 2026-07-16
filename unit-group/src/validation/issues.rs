use crate::models::Severity;

// Description strings are named constants — not literals duplicated between
// the check that raises an issue (mod.rs) and `correctable_fields_for` below
// — so a typo in one copy can't silently desync a check from its
// correctable-fields entry.
pub const BLANK_UNITGROUP: &str = "Blank UnitGroup values";
pub const SUSPICIOUS_UNITGROUP: &str = "Suspicious UnitGroup values";
pub const DUPLICATE_UNITS: &str = "Duplicate unit numbers";
pub const INVALID_DIMENSIONS: &str = "Invalid dimensions";
pub const CLIMATE_MISMATCH: &str = "Climate status does not match UnitGroup";
pub const LOCALITY_MISMATCH: &str = "Locality does not match UnitGroup";
pub const UNITGROUP_DIMENSION_MISMATCH: &str =
    "UnitGroup dimensions do not match Width/Length";
pub const RARE_GROUP: &str = "Rare UnitGroup detected";
pub const SINGLE_UNIT_GROUP: &str = "UnitGroup contains only one unit";
pub const INCONSISTENT_CASING: &str = "Inconsistent unit-number casing";

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

        BLANK_UNITGROUP
        | SUSPICIOUS_UNITGROUP => {
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
    /// per-group checks (rare/single-unit groups). Reused across both
    /// because callers only ever surface the count today (see
    /// api::validate's `affected_units`), not the identifiers
    /// themselves — if that changes, this should split into a proper
    /// `enum FlaggedValues { Units(Vec<String>), Groups(Vec<String>) }`.
    pub flagged_values: Vec<String>,
    pub description: String,
    pub severity: Severity,
}

/// Turns a fixed list of (flagged values, description, severity)
/// candidates into the issues that actually have something to report —
/// i.e. drops any candidate whose list came back empty.
pub(super) fn build<const N: usize>(
    candidates: [(
        Vec<String>,
        &str,
        Severity,
    ); N],
) -> Vec<ValidationIssue> {
    candidates
        .into_iter()
        .filter(|(flagged_values, _, _)| {
            !flagged_values.is_empty()
        })
        .map(
            |(
                flagged_values,
                description,
                severity,
            )| {
                ValidationIssue {
                    flagged_values,
                    description:
                        description
                            .to_string(),
                    severity,
                }
            },
        )
        .collect()
}
