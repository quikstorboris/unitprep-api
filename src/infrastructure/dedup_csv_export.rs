//! Generates the duplicate-tenant-check CSV export, entirely in memory.
//! Kept separate from `csv_export.rs`, which is UnitGroup's own export
//! artifact generation — a new tool gets its own export file rather than
//! being folded into an existing one.
//!
//! Shape: one column set close to the reference script's own output
//! (`CustNumb` plus every field this tool actually reads), flagged
//! groups first — one row per record, blank-row-separated between
//! groups, the correction note on each group's first row only, matching
//! the reference script's own convention — followed by a typo/name-variant
//! section (this crate's addition, since the reference script never
//! wrote these to its CSV output at all). Both sections list every
//! finding; there's no corrective action or confirm/dismiss state to
//! encode, per the tool's MVP scope.

use std::io::Write;

use anyhow::Result;
use csv::Writer;

use unitprep_dedup::comparison::find_differing_categories;
use unitprep_dedup::grouping::group_records;
use unitprep_dedup::types::{
    FieldCategory, FieldMismatch, FieldName, FlaggedGroup, TenantGroup, TenantRecord,
    TypoVariantCandidate,
};
use unitprep_dedup::DedupReport;

const COLUMNS: &[&str] = &[
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

/// Builds the full CSV export for a `DedupReport`. `all_records` is the
/// session's originally ingested records — needed because
/// `TypoVariantCandidate` only carries group keys, not the underlying
/// tenant records; re-grouping here (cheap at current data volumes,
/// same as the matching pass itself) avoids pushing export-shaped data
/// into `unitprep-dedup`, which stays pure domain logic.
pub fn generate_csv(report: &DedupReport, all_records: &[TenantRecord]) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    {
        let mut writer = Writer::from_writer(&mut buffer);
        writer.write_record(COLUMNS)?;
        let mut row_num = 2usize; // row 1 is the header just written

        write_flagged_groups(&mut writer, &report.flagged_groups, &mut row_num)?;

        if !report.typo_variant_candidates.is_empty() {
            write_typo_variant_section(
                &mut writer,
                &report.typo_variant_candidates,
                all_records,
                &mut row_num,
            )?;
        }

        writer.flush()?;
    }
    Ok(buffer)
}

fn write_flagged_groups(
    writer: &mut Writer<impl Write>,
    groups: &[FlaggedGroup],
    row_num: &mut usize,
) -> Result<()> {
    for (i, flagged) in groups.iter().enumerate() {
        if i > 0 {
            write_blank_row(writer)?;
            *row_num += 1;
        }
        let cite_fields = cite_fields_for_mismatches(&flagged.mismatches);
        let note =
            note_with_cell_refs(&flagged.note, &flagged.group.records, &cite_fields, *row_num);
        write_group_rows(writer, &flagged.group, &note, row_num)?;
    }
    Ok(())
}

fn write_typo_variant_section(
    writer: &mut Writer<impl Write>,
    candidates: &[TypoVariantCandidate],
    all_records: &[TenantRecord],
    row_num: &mut usize,
) -> Result<()> {
    let groups = group_records(all_records.to_vec());
    let find = |key: &str| groups.iter().find(|g| g.key == key);

    write_blank_row(writer)?;
    *row_num += 1;
    writer.write_record(marker_row("Possible name/typo variants — for your review"))?;
    *row_num += 1;

    for (i, candidate) in candidates.iter().enumerate() {
        if i > 0 {
            write_blank_row(writer)?;
            *row_num += 1;
        }

        let pair: Vec<&TenantGroup> =
            [find(&candidate.key_a), find(&candidate.key_b)].into_iter().flatten().collect();
        let combined_records: Vec<TenantRecord> =
            pair.iter().flat_map(|group| group.records.clone()).collect();
        let cite_fields = typo_variant_cite_fields(candidate, &combined_records);
        let note =
            note_with_cell_refs(&candidate.note, &combined_records, &cite_fields, *row_num);

        let mut wrote_note = false;
        for group in &pair {
            let row_note = if wrote_note { "" } else { note.as_str() };
            write_group_rows(writer, group, row_note, row_num)?;
            wrote_note = true;
        }
    }
    Ok(())
}

/// One row per record in `group`; `note` is written on the first row
/// only, matching the reference script's convention.
fn write_group_rows(
    writer: &mut Writer<impl Write>,
    group: &TenantGroup,
    note: &str,
    row_num: &mut usize,
) -> Result<()> {
    for (i, record) in group.records.iter().enumerate() {
        let row_note = if i == 0 { note } else { "" };
        writer.write_record(record_row(record, row_note))?;
        *row_num += 1;
    }
    Ok(())
}

/// Every field named across a flagged group's mismatches, in the same
/// (category-priority, then declaration) order `find_differing_categories`
/// already produced them in — no reordering/dedup needed since each field
/// belongs to exactly one category.
fn cite_fields_for_mismatches(mismatches: &[FieldMismatch]) -> Vec<FieldName> {
    mismatches.iter().flat_map(|m| m.fields.iter().map(|f| f.field)).collect()
}

/// A typo-variant candidate always differs by name (that's the whole
/// premise), so `FirstName`/`LastName` are always cited. If contact info
/// doesn't already match, also cite every other differing field —
/// recomputed here from the combined records since `TypoVariantCandidate`
/// only carries a bool, not the field-level detail (unlike `FlaggedGroup`,
/// which already stores its mismatches).
fn typo_variant_cite_fields(
    candidate: &TypoVariantCandidate,
    combined_records: &[TenantRecord],
) -> Vec<FieldName> {
    let mut fields = vec![FieldName::FirstName, FieldName::LastName];
    if !candidate.contact_info_matches {
        for mismatch in find_differing_categories(combined_records) {
            if mismatch.category != FieldCategory::Name {
                fields.extend(mismatch.fields.iter().map(|f| f.field));
            }
        }
    }
    fields
}

/// Appends spreadsheet-style cell references to `base_note` — one clause
/// per cited field, e.g. `"AlternateContactPhoneNumber:
/// S7=3605525629, S8=(blank)"` — computed from the *output* CSV's own
/// column layout (`COLUMNS`, not the source file's) and the row numbers
/// these particular records are about to be written at. Mirrors the
/// reference script's own `note_with_refs` bracket format exactly
/// (`note + "  [" + refs + "]"`).
fn note_with_cell_refs(
    base_note: &str,
    records: &[TenantRecord],
    cite_fields: &[FieldName],
    first_row: usize,
) -> String {
    if base_note.is_empty() || cite_fields.is_empty() {
        return base_note.to_string();
    }

    let ref_clauses: Vec<String> = cite_fields
        .iter()
        .filter_map(|field| {
            let column_name = csv_column_name(*field);
            let column_index = COLUMNS.iter().position(|c| *c == column_name)?;
            let letter = col_letter(column_index);

            let cell_refs: Vec<String> = records
                .iter()
                .enumerate()
                .map(|(i, record)| {
                    let raw = record.field(*field).trim();
                    let value = if raw.is_empty() { "(blank)" } else { raw };
                    format!("{letter}{}={value}", first_row + i)
                })
                .collect();

            Some(format!("{column_name}: {}", cell_refs.join(", ")))
        })
        .collect();

    if ref_clauses.is_empty() {
        base_note.to_string()
    } else {
        format!("{base_note}  [{}]", ref_clauses.join("; "))
    }
}

/// 0-based column index -> spreadsheet column letter(s) (0 -> A, 25 ->
/// Z, 26 -> AA, ...). Mirrors the reference script's own `col_letter`.
fn col_letter(index0: usize) -> String {
    let mut idx = index0 + 1;
    let mut letters = String::new();
    while idx > 0 {
        let rem = (idx - 1) % 26;
        letters.insert(0, (b'A' + rem as u8) as char);
        idx = (idx - 1) / 26;
    }
    letters
}

/// Maps this crate's internal `FieldName` to the export CSV's own column
/// name — they diverge for alternate-contact fields (`AltContact*`
/// internally vs. `AlternateContact*` in the output header), so this
/// can't just be `format!("{field:?}")` the way the note-composer's
/// plain-English detail can.
fn csv_column_name(field: FieldName) -> &'static str {
    match field {
        FieldName::PhoneNumber => "PhoneNumber",
        FieldName::PhoneNumberPrefix => "PhoneNumberPrefix",
        FieldName::Email => "Email",
        FieldName::AddressStreet1 => "AddressStreet1",
        FieldName::AddressStreet2 => "AddressStreet2",
        FieldName::AddressCity => "AddressCity",
        FieldName::AddressState => "AddressState",
        FieldName::AddressPostalCode => "AddressPostalCode",
        FieldName::AltContactFirstName => "AlternateContactFirstName",
        FieldName::AltContactLastName => "AlternateContactLastName",
        FieldName::AltContactEmail => "AlternateContactEmail",
        FieldName::AltContactPhoneNumber => "AlternateContactPhoneNumber",
        FieldName::AltContactPhoneNumberPrefix => "AlternateContactPhoneNumberPrefix",
        FieldName::AltContactAddressStreet1 => "AlternateContactAddressStreet1",
        FieldName::AltContactAddressStreet2 => "AlternateContactAddressStreet2",
        FieldName::AltContactAddressCity => "AlternateContactAddressCity",
        FieldName::AltContactAddressState => "AlternateContactAddressState",
        FieldName::AltContactAddressPostalCode => "AlternateContactAddressPostalCode",
        FieldName::CompanyName => "CompanyName",
        FieldName::FirstName => "FirstName",
        FieldName::LastName => "LastName",
    }
}

fn write_blank_row(writer: &mut Writer<impl Write>) -> Result<()> {
    writer.write_record(std::iter::repeat_n("", COLUMNS.len()))?;
    Ok(())
}

fn marker_row(text: &str) -> Vec<&str> {
    let mut row = vec![text];
    row.extend(std::iter::repeat_n("", COLUMNS.len() - 1));
    row
}

fn record_row<'a>(record: &'a TenantRecord, note: &'a str) -> Vec<&'a str> {
    vec![
        &record.cust_numb,
        &record.unit_number,
        note,
        &record.first_last,
        &record.first_name,
        &record.last_name,
        &record.company_name,
        &record.phone_number_prefix,
        &record.phone_number,
        &record.email,
        &record.address_street1,
        &record.address_street2,
        &record.address_city,
        &record.address_state,
        &record.address_postal_code,
        &record.alt_contact_first_name,
        &record.alt_contact_last_name,
        &record.alt_contact_email,
        &record.alt_contact_phone_number_prefix,
        &record.alt_contact_phone_number,
        &record.alt_contact_address_street1,
        &record.alt_contact_address_street2,
        &record.alt_contact_address_city,
        &record.alt_contact_address_state,
        &record.alt_contact_address_postal_code,
    ]
}

#[cfg(test)]
#[path = "dedup_csv_export_tests.rs"]
mod tests;
