//! Shared row/column planning for the dedup CSV and xlsx exporters —
//! both serialize the *exact same content* (the same three sections,
//! the same row order, the same notes with cell references), so this
//! is the one place that decides what goes where. The two file-format
//! writers (`dedup_csv_export.rs`, `dedup_xlsx_export.rs`) each just
//! walk the plan this module builds and serialize it differently —
//! neither owns any layout decisions of its own, so they can't
//! silently drift apart from each other the way independently-written
//! copies of the same logic have before in this project.

use unitprep_dedup::grouping::group_records;
use unitprep_dedup::types::{TenantGroup, TenantRecord};
use unitprep_dedup::DedupReport;

use cell_refs::{cite_fields_for_mismatches, first_cell_ref, note_with_cell_refs, typo_variant_cite_fields};

mod cell_refs;

// Re-exported at this module's level rather than left `pub(crate)` only
// inside the private `cell_refs` submodule — a private `mod cell_refs;`
// means the submodule path itself isn't nameable from outside
// `dedup_export_plan`, regardless of the item's own visibility. `api::
// dedup_view` needs this to attach real cell references to on-screen
// note bullets (see its own doc comment for why).
pub(crate) use cell_refs::field_cell_refs;

pub const COLUMNS: &[&str] = &[
    "CustNumb",
    "UnitNumber",
    "CorrectionNote",
    "FirtLast",
    "FirstName",
    "LastName",
    "CompanyName",
    "PhoneNumberPrefix",
    "PhoneNumber",
    "Email",
    "AddressStreet1",
    "AddressStreet2",
    "AddressCity",
    "AddressState",
    "AddressPostalCode",
    "AlternateContactFirstName",
    "AlternateContactLastName",
    "AlternateContactEmail",
    "AlternateContactPhoneNumberPrefix",
    "AlternateContactPhoneNumber",
    "AlternateContactAddressStreet1",
    "AlternateContactAddressStreet2",
    "AlternateContactAddressCity",
    "AlternateContactAddressState",
    "AlternateContactAddressPostalCode",
];

/// One line of the exported file, in final write order. `Blank` and
/// `Marker` each still occupy exactly one spreadsheet row (matters for
/// both writers: csv writes a row regardless, xlsx needs to advance
/// its row cursor by one either way).
///
/// `Data` owns its `TenantRecord` rather than borrowing it — the
/// typo-variant and related-tenant sections re-derive their groups
/// from a fresh `group_records` call (since `TypoVariantCandidate`/
/// `RelatedTenantCandidate` only carry group keys, not records), which
/// produces owned data with no lifetime tying it back to the
/// original `all_records` slice. Cloning here is the same cost this
/// crate already pays in several other places (e.g. `group.records
/// .clone()`); at real facility data volumes this is not a concern.
pub enum PlannedRow {
    Data {
        // Boxed: `Data`'s other fields are small, but `TenantRecord`
        // itself is a ~600-byte struct of 24 owned Strings, and every
        // enum instance costs as much stack space as its largest
        // variant — boxing keeps `Blank`/`Marker` (a handful of bytes)
        // from being padded out to match.
        record: Box<TenantRecord>,
        /// Empty unless this is the first row of its group/candidate —
        /// the note is written once per cluster, matching the
        /// reference script's own convention.
        note: String,
        /// Increases by one per group/typo-variant/related-tenant
        /// cluster — the xlsx writer cycles a background color by
        /// this so adjacent clusters are easy to tell apart visually.
        /// Unused by the csv writer.
        cluster: usize,
        /// The first cell `note` cites (e.g. `"T7"`), only set
        /// alongside a non-empty note that has at least one citation.
        /// Unused by the csv writer; the xlsx writer turns this into a
        /// clickable internal hyperlink on the note cell.
        hyperlink_target: Option<String>,
    },
    Blank,
    Marker(&'static str),
}

/// Builds the full row plan for `report`. `all_records` is the
/// session's originally ingested records — needed because
/// `TypoVariantCandidate`/`RelatedTenantCandidate` only carry group
/// keys, not the underlying tenant records; re-grouping here (cheap at
/// current data volumes, same as the matching pass itself) avoids
/// pushing export-shaped data into `unitprep-dedup`, which stays pure
/// domain logic.
pub fn build_export_plan(report: &DedupReport, all_records: &[TenantRecord]) -> Vec<PlannedRow> {
    let mut plan = Vec::new();
    let mut row_num = 2usize; // row 1 is the header
    let mut cluster = 0usize;

    for (i, flagged) in report.flagged_groups.iter().enumerate() {
        if i > 0 {
            plan.push(PlannedRow::Blank);
            row_num += 1;
        }

        let cite_fields = cite_fields_for_mismatches(&flagged.mismatches);
        let note = note_with_cell_refs(&flagged.note, &flagged.group.records, &cite_fields, row_num);
        let hyperlink_target = first_cell_ref(&cite_fields, row_num);

        push_group_rows(&mut plan, &flagged.group, note, hyperlink_target, cluster, &mut row_num);
        cluster += 1;
    }

    if !report.typo_variant_candidates.is_empty() {
        plan.push(PlannedRow::Blank);
        row_num += 1;
        plan.push(PlannedRow::Marker("Possible name/typo variants — for your review"));
        row_num += 1;

        let groups = group_records(all_records.to_vec());
        let find = |key: &str| groups.iter().find(|g| g.key == key);

        for (i, candidate) in report.typo_variant_candidates.iter().enumerate() {
            if i > 0 {
                plan.push(PlannedRow::Blank);
                row_num += 1;
            }

            let pair: Vec<&TenantGroup> =
                [find(&candidate.key_a), find(&candidate.key_b)].into_iter().flatten().collect();
            let combined: Vec<TenantRecord> = pair.iter().flat_map(|g| g.records.clone()).collect();
            let cite_fields = typo_variant_cite_fields(candidate, &combined);
            let note = note_with_cell_refs(&candidate.note, &combined, &cite_fields, row_num);
            let hyperlink_target = first_cell_ref(&cite_fields, row_num);

            let mut wrote_note = false;
            for group in &pair {
                let row_note = if wrote_note { String::new() } else { note.clone() };
                let target = if wrote_note { None } else { hyperlink_target.clone() };
                push_group_rows(&mut plan, group, row_note, target, cluster, &mut row_num);
                wrote_note = true;
            }
            cluster += 1;
        }
    }

    if !report.related_tenant_candidates.is_empty() {
        plan.push(PlannedRow::Blank);
        row_num += 1;
        plan.push(PlannedRow::Marker(
            "Possible related tenants (shared contact info, different names) — for your review",
        ));
        row_num += 1;

        let groups = group_records(all_records.to_vec());
        let find = |key: &str| groups.iter().find(|g| g.key == key);

        for (i, candidate) in report.related_tenant_candidates.iter().enumerate() {
            if i > 0 {
                plan.push(PlannedRow::Blank);
                row_num += 1;
            }

            let member_groups: Vec<&TenantGroup> =
                candidate.group_keys.iter().filter_map(|key| find(key)).collect();

            // No cell references for this category — a shared value
            // has no single well-defined differing cell to point at
            // the way a FieldMismatch does, so there's no hyperlink
            // target either.
            let mut wrote_note = false;
            for group in &member_groups {
                let row_note = if wrote_note { String::new() } else { candidate.note.clone() };
                push_group_rows(&mut plan, group, row_note, None, cluster, &mut row_num);
                wrote_note = true;
            }
            cluster += 1;
        }
    }

    plan
}

fn push_group_rows(
    plan: &mut Vec<PlannedRow>,
    group: &TenantGroup,
    note: String,
    hyperlink_target: Option<String>,
    cluster: usize,
    row_num: &mut usize,
) {
    for (i, record) in group.records.iter().enumerate() {
        plan.push(PlannedRow::Data {
            record: Box::new(record.clone()),
            note: if i == 0 { note.clone() } else { String::new() },
            cluster,
            hyperlink_target: if i == 0 { hyperlink_target.clone() } else { None },
        });
        *row_num += 1;
    }
}

#[cfg(test)]
#[path = "dedup_export_plan_tests.rs"]
mod tests;
