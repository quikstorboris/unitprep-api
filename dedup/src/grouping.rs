//! Pass 1: group tenant records by exact `FirtLast` match. Fuzzy/typo
//! similarity (see `similarity`) is a separate, advisory-only pass —
//! never used to decide group membership here, per UnitPrep's
//! exact-match-decides principle.

use crate::normalization::collapse_whitespace;
use crate::types::{TenantGroup, TenantRecord};

/// Grouping key: trim + lowercase + internal-whitespace-collapse of the
/// raw `FirtLast` value. Every other `Plain`-kind field (see
/// `normalization::normalize_value`) already collapses repeated internal
/// whitespace, not just leading/trailing -- this key was trim-only, so
/// "John  Smith" (double space) and "John Smith" produced two different
/// group keys instead of exact-matching into one group.
pub fn group_key(first_last: &str) -> String {
    collapse_whitespace(&first_last.trim().to_lowercase())
}

/// Groups records by name (`group_key`), preserving first-seen order
/// (mirrors the reference script's use of `OrderedDict` — matters for
/// stable, reproducible output ordering, not for correctness of the
/// grouping itself).
///
/// This is the *name* view of the data. Typo-variant and related-tenant
/// detection compare people by name and so run on these groups; the
/// tenant counts and the contact-mismatch check use
/// `group_records_by_tenant`, which prefers the vendor's own tenant id.
///
/// A blank `FirtLast` never merges with another blank one: two tenants
/// who both left this field empty (e.g. manual/walk-in entries) are not
/// thereby the same tenant, so each blank-keyed record gets its own
/// singleton group instead of being pooled into one shared `""` bucket
/// that would otherwise report a pile of unrelated contact-info
/// "mismatches" between strangers.
pub fn group_records(records: Vec<TenantRecord>) -> Vec<TenantGroup> {
    group_with(records, |record| group_key(&record.first_last))
}

/// Key for a group of records that share a vendor tenant id. The prefix
/// keeps it from ever colliding with a name key.
pub fn tenant_id_key(tenant_id: &str) -> Option<String> {
    let id = tenant_id.trim();
    (!id.is_empty()).then(|| format!("tenant:{id}"))
}

/// Groups records into *tenants*: by the vendor's own tenant id when the
/// record has one, falling back to the name key when it doesn't. A
/// vendor id says "same contact record" far more reliably than a name
/// does: two records under one id can never disagree about contact info,
/// and one name under two ids is a duplicate customer record
/// (`duplicate_records`), not a single tenant.
pub fn group_records_by_tenant(records: Vec<TenantRecord>) -> Vec<TenantGroup> {
    group_with(records, |record| {
        tenant_id_key(&record.tenant_id).unwrap_or_else(|| group_key(&record.first_last))
    })
}

/// Shared grouping loop. `key_for` returns an empty string for a record
/// with no usable key, which then becomes its own singleton group.
fn group_with(
    records: Vec<TenantRecord>,
    key_for: impl Fn(&TenantRecord) -> String,
) -> Vec<TenantGroup> {
    let mut groups: Vec<TenantGroup> = Vec::new();
    let mut blank_key_sequence = 0usize;
    for record in records {
        let key = key_for(&record);
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

    /// Regression test: repeated internal whitespace must collapse the
    /// same way every other Plain-kind field's normalization does, so
    /// "John  Smith" (double space) exact-matches "John Smith" into one
    /// group instead of silently landing in two.
    #[test]
    fn internal_whitespace_variance_still_groups_together() {
        let groups = group_records(vec![
            record("John  Smith", "A1"),
            record("John Smith", "A2"),
        ]);

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].records.len(), 2);
    }

    #[test]
    fn blank_key_groups_are_not_multi_unit() {
        let groups = group_records(vec![record("", "A1"), record("", "A2")]);

        assert!(multi_unit_groups(groups).is_empty());
    }
}
