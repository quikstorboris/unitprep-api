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
pub fn group_records(records: Vec<TenantRecord>) -> Vec<TenantGroup> {
    let mut groups: Vec<TenantGroup> = Vec::new();
    for record in records {
        let key = group_key(&record.first_last);
        match groups.iter_mut().find(|g| g.key == key) {
            Some(group) => group.records.push(record),
            None => groups.push(TenantGroup { key, records: vec![record] }),
        }
    }
    groups
}

/// Multi-unit tenants only (2+ records) — the reference script's
/// `multi`. Single-unit tenants are never flagged or compared.
pub fn multi_unit_groups(groups: Vec<TenantGroup>) -> Vec<TenantGroup> {
    groups.into_iter().filter(|g| g.records.len() >= 2).collect()
}
