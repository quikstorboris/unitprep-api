//! The export section for tenants the source gave no customer id (see
//! `unitprep_dedup::unidentified`). Same row mechanics as the other
//! sections; notes carry no cell references because these rows are
//! reported for review, not pinned to one differing cell.

use unitprep_dedup::types::{TenantGroup, TenantRecord};
use unitprep_dedup::{DedupReport, UnidentifiedMode};

use super::{push_group_rows, PlannedRow};

pub(super) fn push_section(
    plan: &mut Vec<PlannedRow>,
    row_num: &mut usize,
    cluster: &mut usize,
    report: &DedupReport,
    groups: &[TenantGroup],
) {
    let Some(section) = report.unidentified.as_ref() else {
        return;
    };
    if section.tenants.is_empty() {
        return;
    }

    let find = |key: &str| groups.iter().find(|g| g.key == key);

    plan.push(PlannedRow::Blank);
    *row_num += 1;
    plan.push(PlannedRow::Marker(
        "Tenants without a customer ID — for your review",
    ));
    *row_num += 1;

    for (i, tenant) in section.tenants.iter().enumerate() {
        if i > 0 {
            plan.push(PlannedRow::Blank);
            *row_num += 1;
        }

        // Only this tenant's own id-less rows; a same-name tenant that has
        // an id is a separate cluster elsewhere.
        let records: Vec<TenantRecord> = find(&tenant.key)
            .map(|g| {
                g.records
                    .iter()
                    .filter(|r| r.tenant_id.trim().is_empty())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if records.is_empty() {
            continue;
        }

        let note = match section.mode {
            UnidentifiedMode::MatchedByName if !tenant.same_name_as.is_empty() => {
                let ids: Vec<String> = tenant
                    .same_name_as
                    .iter()
                    .map(|m| format!("customer ID {} (unit {})", m.tenant_id, m.units.join(", ")))
                    .collect();
                format!(
                    "No customer ID in the source file. The same name appears under {} — likely the same customer.",
                    ids.join("; ")
                )
            }
            UnidentifiedMode::MatchedByName => {
                "No customer ID in the source file, and no other tenant has this exact name."
                    .to_string()
            }
            _ => "No customer ID in the source file, so this tenant was not checked against the others."
                .to_string(),
        };

        let group = TenantGroup {
            key: tenant.key.clone(),
            records,
        };
        push_group_rows(plan, &group, note, None, *cluster, row_num);
        *cluster += 1;
    }

    // What matching by name found among everyone, when the user asked.
    for (i, flagged) in section.flagged_groups.iter().enumerate() {
        plan.push(PlannedRow::Blank);
        *row_num += 1;
        if i == 0 {
            plan.push(PlannedRow::Marker(
                "Matched by name — contact details that differ",
            ));
            *row_num += 1;
        }
        push_group_rows(
            plan,
            &flagged.group,
            flagged.note.clone(),
            None,
            *cluster,
            row_num,
        );
        *cluster += 1;
    }

    let mut first_variant = true;
    for candidate in &section.typo_variant_candidates {
        let pair: Vec<&TenantGroup> = [find(&candidate.key_a), find(&candidate.key_b)]
            .into_iter()
            .flatten()
            .collect();
        plan.push(PlannedRow::Blank);
        *row_num += 1;
        if first_variant {
            plan.push(PlannedRow::Marker(
                "Matched by name — possible name variants",
            ));
            *row_num += 1;
            first_variant = false;
        }
        let mut wrote_note = false;
        for group in pair {
            let row_note = if wrote_note {
                String::new()
            } else {
                candidate.note.clone()
            };
            push_group_rows(plan, group, row_note, None, *cluster, row_num);
            wrote_note = true;
        }
        *cluster += 1;
    }

    let mut first_related = true;
    for candidate in &section.related_tenant_candidates {
        plan.push(PlannedRow::Blank);
        *row_num += 1;
        if first_related {
            plan.push(PlannedRow::Marker(
                "Matched by name — possible related tenants",
            ));
            *row_num += 1;
            first_related = false;
        }
        let mut wrote_note = false;
        for key in &candidate.group_keys {
            let Some(group) = find(key) else { continue };
            let row_note = if wrote_note {
                String::new()
            } else {
                candidate.note.clone()
            };
            push_group_rows(plan, group, row_note, None, *cluster, row_num);
            wrote_note = true;
        }
        *cluster += 1;
    }
}
