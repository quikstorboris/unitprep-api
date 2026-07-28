//! Pass 1: group tenant records by exact `FirtLast` match. Fuzzy/typo
//! similarity (see `similarity`) is a separate, advisory-only pass —
//! never used to decide group membership here, per UnitPrep's
//! exact-match-decides principle.

use crate::types::{TenantGroup, TenantRecord};

/// Grouping key: trim + lowercase of the raw `FirtLast` value. Matches
/// the reference script's `group_key`.
pub fn group_key(first_last: &str) -> String {
    first_last.trim().to_lowercase()
}

/// Groups records by `group_key`, preserving first-seen order (mirrors
/// the reference script's use of `OrderedDict` — matters for stable,
/// reproducible output ordering, not for correctness of the grouping
/// itself).
///
/// A blank `FirtLast` never merges with another blank one: two tenants
/// who both left this field empty (e.g. manual/walk-in entries) are not
/// thereby the same tenant, so each blank-keyed record gets its own
/// singleton group instead of being pooled into one shared `""` bucket
/// that would otherwise report a pile of unrelated contact-info
/// "mismatches" between strangers.
pub fn group_records(records: Vec<TenantRecord>) -> Vec<TenantGroup> {
    let mut groups: Vec<TenantGroup> = Vec::new();
    let mut blank_key_sequence = 0usize;
    for record in records {
        let key = group_key(&record.first_last);
        if key.is_empty() {
            blank_key_sequence += 1;
            groups.push(TenantGroup {
                key: format!("__blank_{blank_key_sequence}__"),
                records: vec![record],
            });
            continue;
        }
        match groups.iter_mut().find(|g| g.key == key) {
            Some(group) => group.records.push(record),
            None => groups.push(TenantGroup {
                key,
                records: vec![record],
            }),
        }
    }
    groups
}

/// Multi-unit tenants only (2+ records) — the reference script's
/// `multi`. Single-unit tenants are never flagged or compared.
pub fn multi_unit_groups(groups: Vec<TenantGroup>) -> Vec<TenantGroup> {
    groups
        .into_iter()
        .filter(|g| g.records.len() >= 2)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(first_last: &str, unit_number: &str) -> TenantRecord {
        TenantRecord {
            first_last: first_last.to_string(),
            unit_number: unit_number.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn same_key_records_group_together() {
        let groups = group_records(vec![record("John Smith", "A1"), record("john smith", "A2")]);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].records.len(), 2);
    }

    #[test]
    fn blank_keys_never_merge_with_each_other() {
        let groups = group_records(vec![
            record("", "A1"),
            record("   ", "A2"),
            record("Jane Doe", "A3"),
        ]);

        // Two singleton blank-key groups, plus the real "jane doe" group —
        // never one shared "" bucket holding two unrelated tenants.
        assert_eq!(groups.len(), 3);
        assert!(
            groups
                .iter()
                .filter(|g| g.records.len() == 1 && g.records[0].first_last.trim().is_empty())
                .count()
                == 2
        );
    }

    #[test]
    fn blank_key_groups_are_not_multi_unit() {
        let groups = group_records(vec![record("", "A1"), record("", "A2")]);

        assert!(multi_unit_groups(groups).is_empty());
    }
}
