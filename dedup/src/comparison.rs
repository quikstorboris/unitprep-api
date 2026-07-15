//! Pass 2: within a multi-unit group, find which contact-info
//! categories disagree. Ported from the reference script's
//! `find_differing_categories` / `contact_info_matches`.

use crate::normalization::normalize_value;
use crate::types::{FieldMismatch, FieldName, TenantRecord, CATEGORY_PRIORITY, FIELD_SPECS};

/// For each field category (in priority order), checks whether any of
/// its fields differ (after normalization) across `group`. Returns one
/// `FieldMismatch` per category that has at least one differing field,
/// in priority order — mirrors the reference script's `differing` /
/// `differing_fields`. Blank vs. filled counts as differing (an
/// incomplete record is a mismatch, not a match), same as the
/// reference script's plain set-based comparison.
pub fn find_differing_categories(group: &[TenantRecord]) -> Vec<FieldMismatch> {
    let mut result = Vec::new();
    for category in CATEGORY_PRIORITY {
        let differing_fields: Vec<FieldName> = FIELD_SPECS
            .iter()
            .filter(|spec| spec.category == category)
            .filter(|spec| !field_matches_across(group, spec.name, spec.kind))
            .map(|spec| spec.name)
            .collect();
        if !differing_fields.is_empty() {
            result.push(FieldMismatch { category, fields: differing_fields });
        }
    }
    result
}

/// True if every non-`Name`-category field already matches (after
/// normalization) across `group`. Used by the typo-variant pass to
/// decide the confirmation note's wording, not whether to surface a
/// candidate (see crate-level docs: always flag).
pub fn contact_info_matches(group: &[TenantRecord]) -> bool {
    FIELD_SPECS
        .iter()
        .filter(|spec| !matches!(spec.category, crate::types::FieldCategory::Name))
        .all(|spec| field_matches_across(group, spec.name, spec.kind))
}

fn field_matches_across(
    group: &[TenantRecord],
    name: FieldName,
    kind: crate::types::FieldKind,
) -> bool {
    let mut values = group
        .iter()
        .map(|r| normalize_value(kind, r.field(name)));
    let first = match values.next() {
        Some(v) => v,
        None => return true,
    };
    values.all(|v| v == first)
}
