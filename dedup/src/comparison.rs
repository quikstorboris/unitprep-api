//! Pass 2: within a multi-unit group, find which contact-info
//! categories disagree. Ported from the reference script's
//! `find_differing_categories` / `contact_info_matches`.

use crate::normalization::normalize_value;
use crate::types::{FieldMismatch, FieldName, FieldValueMismatch, TenantRecord, CATEGORY_PRIORITY, FIELD_SPECS};

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
        let differing_fields: Vec<FieldValueMismatch> = FIELD_SPECS
            .iter()
            .filter(|spec| spec.category == category)
            .filter(|spec| !field_matches_across(group, spec.name, spec.kind))
            .map(|spec| FieldValueMismatch {
                field: spec.name,
                values: distinct_display_values(group, spec.name),
            })
            .collect();
        if !differing_fields.is_empty() {
            result.push(FieldMismatch { category, fields: differing_fields });
        }
    }
    result
}

/// Distinct raw (trimmed, not normalized) values for `field` across
/// `group`, blank shown as `"(blank)"`, sorted with blank last — same
/// display convention as the reference script's console summary
/// (`sorted({...}, key=lambda x: (x == "(blank)", x))`).
fn distinct_display_values(group: &[TenantRecord], field: FieldName) -> Vec<String> {
    let mut values: Vec<String> = group
        .iter()
        .map(|r| {
            let raw = r.field(field).trim();
            if raw.is_empty() { "(blank)".to_string() } else { raw.to_string() }
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    values.sort_by_key(|v| (v == "(blank)", v.clone()));
    values
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::FieldCategory;

    fn record(email: &str) -> TenantRecord {
        TenantRecord { email: email.to_string(), ..Default::default() }
    }

    #[test]
    fn differing_field_carries_actual_distinct_values() {
        let group = vec![record("a@example.com"), record("")];
        let differing = find_differing_categories(&group);

        let email_mismatch = differing
            .iter()
            .find(|m| m.category == FieldCategory::Email)
            .expect("email should be flagged as differing");

        assert_eq!(email_mismatch.fields.len(), 1);
        assert_eq!(email_mismatch.fields[0].field, FieldName::Email);
        // Blank sorts last, matching the reference script's display convention.
        assert_eq!(email_mismatch.fields[0].values, vec!["a@example.com", "(blank)"]);
    }

    #[test]
    fn matching_field_is_not_reported_as_differing() {
        let group = vec![record("same@example.com"), record("same@example.com")];
        let differing = find_differing_categories(&group);
        assert!(differing.iter().all(|m| m.category != FieldCategory::Email));
    }
}
