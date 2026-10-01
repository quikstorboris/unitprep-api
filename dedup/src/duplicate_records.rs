//! The inverse of a flagged group: ONE PERSON under SEVERAL customer
//! records.
//!
//! A flagged group is one tenant whose units disagree about contact
//! info. When the export carries the vendor's own tenant id, the more
//! common data problem is different: the same person was entered twice,
//! once per unit, so each record gets its own tenant id. Those records
//! typically agree on every contact field, so the comparison pass finds
//! nothing in them and they would never surface. This pass finds them by
//! grouping the tenant-id groups by normalized name and reporting any
//! name held by two or more distinct tenant ids.
//!
//! It only looks at records that carry a tenant id: a format without one
//! (QSX, Easy Storage Solutions) already groups by name, so there is
//! nothing to compare. Any contact differences between the duplicate
//! records are reported alongside (they used to surface as a flagged
//! group when the grouping was by name).

use serde::{Deserialize, Serialize};

use crate::comparison::find_differing_categories;
use crate::grouping::group_key;
use crate::phrasing::{group_units, oxford_join, units_phrase};
use crate::types::{FieldCategory, FieldMismatch, TenantGroup, TenantRecord};

/// One of the customer records behind a duplicated name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateTenantEntry {
    pub tenant_id: String,
    pub units: Vec<String>,
    pub records: Vec<TenantRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateCustomerRecord {
    /// The normalized name the records share (`grouping::group_key`).
    pub name_key: String,
    pub display_name: String,
    /// One entry per distinct tenant id, in first-seen order.
    pub tenants: Vec<DuplicateTenantEntry>,
    /// Contact categories that differ across the combined records; empty
    /// when they are identical, the common case.
    pub mismatches: Vec<FieldMismatch>,
    pub note: String,
}

/// Names held by two or more distinct tenant ids, from the tenant-keyed
/// groups (`grouping::group_records_by_tenant`). Order follows the first
/// appearance of each name.
pub fn find_duplicate_customer_records(
    tenant_groups: &[TenantGroup],
) -> Vec<DuplicateCustomerRecord> {
    let mut by_name: Vec<(String, Vec<&TenantGroup>)> = Vec::new();

    for group in tenant_groups {
        let Some(first) = group.records.first() else {
            continue;
        };
        if first.tenant_id.trim().is_empty() {
            continue;
        }
        let name_key = group_key(&first.first_last);
        if name_key.is_empty() {
            continue;
        }
        match by_name.iter_mut().find(|(key, _)| *key == name_key) {
            Some((_, groups)) => groups.push(group),
            None => by_name.push((name_key, vec![group])),
        }
    }

    by_name
        .into_iter()
        .filter_map(|(name_key, groups)| build_finding(name_key, &groups))
        .collect()
}

fn build_finding(name_key: String, groups: &[&TenantGroup]) -> Option<DuplicateCustomerRecord> {
    let tenants: Vec<DuplicateTenantEntry> = groups
        .iter()
        .map(|group| {
            let tenant_id = group.records[0].tenant_id.trim().to_string();
            // `group_units` is sorted, so equal units are adjacent. A unit
            // label that just repeats the tenant id (a format with no real
            // unit column, mapped to the id) says nothing, so it drops out.
            let mut units: Vec<String> = group_units(group)
                .into_iter()
                .filter(|unit| *unit != tenant_id)
                .map(String::from)
                .collect();
            units.dedup();
            DuplicateTenantEntry {
                tenant_id,
                units,
                records: group.records.clone(),
            }
        })
        .collect();

    let mut distinct_ids: Vec<&str> = tenants.iter().map(|t| t.tenant_id.as_str()).collect();
    distinct_ids.sort_unstable();
    distinct_ids.dedup();
    if distinct_ids.len() < 2 {
        return None;
    }

    let combined: Vec<TenantRecord> = tenants.iter().flat_map(|t| t.records.clone()).collect();
    let mismatches = find_differing_categories(&combined);
    let display_name = combined[0].display_name();
    let note = compose_note(&display_name, &tenants, &mismatches);

    Some(DuplicateCustomerRecord {
        name_key,
        display_name,
        tenants,
        mismatches,
        note,
    })
}

fn category_phrase(category: FieldCategory) -> &'static str {
    match category {
        FieldCategory::Phone => "phone number",
        FieldCategory::Email => "email address",
        FieldCategory::Address => "address",
        FieldCategory::AltContact => "alternate contact",
        FieldCategory::Company => "company name",
        FieldCategory::Name => "name",
    }
}

fn compose_note(
    name: &str,
    tenants: &[DuplicateTenantEntry],
    mismatches: &[FieldMismatch],
) -> String {
    let records: Vec<String> = tenants
        .iter()
        .map(|t| {
            let units: Vec<&str> = t.units.iter().map(String::as_str).collect();
            if units.is_empty() {
                format!("ID {}", t.tenant_id)
            } else {
                format!("ID {} ({})", t.tenant_id, units_phrase(&units))
            }
        })
        .collect();
    let listed: Vec<&str> = records.iter().map(String::as_str).collect();

    let lead = format!(
        "{name} has {} separate customer records: {}.",
        tenants.len(),
        oxford_join(&listed)
    );

    if mismatches.is_empty() {
        format!(
            "{lead} The contact details match, so these can be merged into one customer record."
        )
    } else {
        let differing: Vec<&str> = mismatches
            .iter()
            .map(|m| category_phrase(m.category))
            .collect();
        format!(
            "{lead} The contact details also differ ({}); merge them into one customer record and correct the details.",
            oxford_join(&differing)
        )
    }
}

#[cfg(test)]
#[path = "duplicate_records_tests.rs"]
mod tests;
