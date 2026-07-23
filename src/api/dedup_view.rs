//! Enriches `unitprep_dedup`'s pure `DedupReport` into the richer shape
//! the on-screen UI actually renders — real display names (not raw
//! grouping keys), plain-English per-field sentences instead of one
//! dense paragraph, and cell references into the *exported* CSV/xlsx
//! (confirmed with Boris — not the source file) so a reader can jump
//! straight to the cell that needs fixing.
//!
//! This lives here, not in `unitprep_dedup` itself, because it needs
//! `dedup_export_plan`'s column-layout/cell-ref concept — deliberately
//! kept out of the pure domain crate (see that module's own doc
//! comment). `build_export_plan` is already fully deterministic given
//! `report` + `records` (no I/O), so calling it here at report-request
//! time — before any real export happens — is safe and guaranteed to
//! land on the exact same cells a subsequent `/dedup/export` would
//! produce, since both call the same planner.

use std::collections::BTreeMap;

use serde::Serialize;

use unitprep_dedup::grouping::group_records;
use unitprep_dedup::types::{FieldCategory, FieldName, TenantRecord};
use unitprep_dedup::{group_units, human_label, DedupReport, NoteComposer, RelatednessSignal, TemplateNoteComposer};

use crate::infrastructure::dedup_export_plan::{build_export_plan, field_cell_refs, PlannedRow};

#[derive(Debug, Clone, Serialize)]
pub struct BulletView {
    pub field: FieldName,
    pub label: &'static str,
    pub sentence: String,
    /// Cell references in the *exported* CSV/xlsx — empty if the export
    /// planner didn't place this group anywhere (shouldn't happen for a
    /// group that's actually in `report.flagged_groups`, but stays
    /// empty rather than fabricating a reference if it ever does).
    pub cell_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FlaggedGroupView {
    pub key: String,
    pub display_name: String,
    pub units: Vec<String>,
    /// In `CATEGORY_PRIORITY` order — `find_differing_categories`
    /// already produces `mismatches` in that order, so this is a plain
    /// projection, not a re-sort.
    pub categories: Vec<FieldCategory>,
    pub bullets: Vec<BulletView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TypoVariantView {
    pub display_name_a: String,
    pub units_a: Vec<String>,
    pub display_name_b: String,
    pub units_b: Vec<String>,
    pub contact_info_matches: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelatedTenantMemberView {
    pub display_name: String,
    pub units: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelatedTenantView {
    pub members: Vec<RelatedTenantMemberView>,
    pub signal: RelatednessSignal,
    pub shared_value: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DedupReportView {
    pub total_rows: usize,
    pub unique_tenants: usize,
    pub multi_unit_tenants: usize,
    pub flagged_groups: Vec<FlaggedGroupView>,
    pub typo_variant_candidates: Vec<TypoVariantView>,
    pub related_tenant_candidates: Vec<RelatedTenantView>,
}

pub fn build_report_view(report: &DedupReport, records: &[TenantRecord]) -> DedupReportView {
    let plan = build_export_plan(report, records);
    let cluster_rows = cluster_first_row_and_count(&plan);

    let composer = TemplateNoteComposer;

    let flagged_groups = report
        .flagged_groups
        .iter()
        .enumerate()
        .map(|(cluster, flagged)| {
            let display_name = flagged.group.records[0].display_name();
            let units: Vec<String> =
                group_units(&flagged.group).into_iter().map(String::from).collect();
            let categories: Vec<FieldCategory> =
                flagged.mismatches.iter().map(|m| m.category).collect();

            let bullets = composer
                .describe_group_bullets(&flagged.group, &flagged.mismatches)
                .into_iter()
                .map(|(field, sentence)| {
                    let cell_refs = cluster_rows
                        .get(&cluster)
                        .map(|&(first_row, record_count)| {
                            field_cell_refs(field, first_row, record_count)
                        })
                        .unwrap_or_default();

                    BulletView { field, label: human_label(field), sentence, cell_refs }
                })
                .collect();

            FlaggedGroupView { key: flagged.group.key.clone(), display_name, units, categories, bullets }
        })
        .collect();

    // Typo-variant and related-tenant candidates only carry group keys
    // (not records) — re-derive groups the same way `dedup_export_plan`
    // already does for this exact lookup, so display names/units can be
    // resolved without pushing this concern into the pure domain crate.
    let groups = group_records(records.to_vec());
    let find = |key: &str| groups.iter().find(|g| g.key == key);

    let typo_variant_candidates = report
        .typo_variant_candidates
        .iter()
        .map(|candidate| {
            let group_a = find(&candidate.key_a);
            let group_b = find(&candidate.key_b);

            TypoVariantView {
                display_name_a: group_a.map(|g| g.records[0].display_name()).unwrap_or_default(),
                units_a: group_a
                    .map(|g| group_units(g).into_iter().map(String::from).collect())
                    .unwrap_or_default(),
                display_name_b: group_b.map(|g| g.records[0].display_name()).unwrap_or_default(),
                units_b: group_b
                    .map(|g| group_units(g).into_iter().map(String::from).collect())
                    .unwrap_or_default(),
                contact_info_matches: candidate.contact_info_matches,
                note: candidate.note.clone(),
            }
        })
        .collect();

    let related_tenant_candidates = report
        .related_tenant_candidates
        .iter()
        .map(|candidate| {
            let members = candidate
                .group_keys
                .iter()
                .map(|key| {
                    let group = find(key);
                    RelatedTenantMemberView {
                        display_name: group.map(|g| g.records[0].display_name()).unwrap_or_default(),
                        units: group
                            .map(|g| group_units(g).into_iter().map(String::from).collect())
                            .unwrap_or_default(),
                    }
                })
                .collect();

            RelatedTenantView {
                members,
                signal: candidate.signal,
                shared_value: candidate.shared_value.clone(),
                note: candidate.note.clone(),
            }
        })
        .collect();

    DedupReportView {
        total_rows: report.total_rows,
        unique_tenants: report.unique_tenants,
        multi_unit_tenants: report.multi_unit_tenants,
        flagged_groups,
        typo_variant_candidates,
        related_tenant_candidates,
    }
}

/// Maps each cluster index to (first row it occupies, how many records
/// it spans) by walking the plan in write order — row 1 is the header,
/// so the Nth plan entry (0-indexed) is row `N + 2`; every `PlannedRow`
/// variant (including `Blank`/`Marker`) occupies exactly one row, so
/// this is a plain running count, not a re-implementation of the
/// planner's own blank/marker bookkeeping. Only flagged-group clusters
/// (index `< report.flagged_groups.len()`) are ever looked up by
/// `build_report_view`, but this doesn't need to know that to stay
/// correct for typo-variant/related-tenant clusters too.
fn cluster_first_row_and_count(plan: &[PlannedRow]) -> BTreeMap<usize, (usize, usize)> {
    let mut rows: BTreeMap<usize, (usize, usize)> = BTreeMap::new();

    for (i, row) in plan.iter().enumerate() {
        if let PlannedRow::Data { cluster, .. } = row {
            let row_num = i + 2;
            let entry = rows.entry(*cluster).or_insert((row_num, 0));
            entry.1 += 1;
        }
    }

    rows
}

#[cfg(test)]
#[path = "dedup_view_tests.rs"]
mod tests;
