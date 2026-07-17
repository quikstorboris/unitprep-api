use std::io::Cursor;

use calamine::{open_workbook_auto_from_rs, Data, Reader};

use super::*;
use unitprep_dedup::types::{FieldCategory, FieldMismatch, FieldName, FieldValueMismatch, FlaggedGroup, TenantGroup};

fn record(unit: &str, alt_phone: &str) -> TenantRecord {
    TenantRecord { unit_number: unit.to_string(), alt_contact_phone_number: alt_phone.to_string(), ..Default::default() }
}

/// Reads the generated workbook's first (only) sheet back as plain
/// strings, header row included — real verification of what actually
/// got written, not just "some bytes came out".
fn read_back(xlsx_bytes: Vec<u8>) -> Vec<Vec<String>> {
    let mut workbook = open_workbook_auto_from_rs(Cursor::new(xlsx_bytes)).expect("valid xlsx");
    let sheet_name = workbook.sheet_names().first().cloned().expect("at least one sheet");
    let range = workbook.worksheet_range(&sheet_name).expect("readable sheet");

    range
        .rows()
        .map(|row| row.iter().map(cell_to_string).collect())
        .collect()
}

fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(v) => v.clone(),
        other => other.to_string(),
    }
}

#[test]
fn generate_xlsx_writes_the_header_row() {
    let report = DedupReport::default();
    let xlsx_bytes = generate_xlsx(&report, &[]).expect("xlsx generation should succeed");
    let rows = read_back(xlsx_bytes);

    assert_eq!(rows[0], COLUMNS.to_vec());
}

#[test]
fn generate_xlsx_writes_flagged_group_rows_and_notes() {
    let group = TenantGroup {
        key: "smith".to_string(),
        records: vec![record("101", ""), record("102", "5551234")],
    };

    let report = DedupReport {
        total_rows: 2,
        unique_tenants: 1,
        multi_unit_tenants: 1,
        flagged_groups: vec![FlaggedGroup {
            group,
            mismatches: vec![FieldMismatch {
                category: FieldCategory::AltContact,
                fields: vec![FieldValueMismatch {
                    field: FieldName::AltContactPhoneNumber,
                    values: vec!["5551234".into(), "(blank)".into()],
                }],
            }],
            note: "Please update the alternate contact info to match across units 101, 102.".to_string(),
        }],
        typo_variant_candidates: vec![],
        related_tenant_candidates: vec![],
    };

    let xlsx_bytes = generate_xlsx(&report, &[]).expect("xlsx generation should succeed");
    let rows = read_back(xlsx_bytes);

    // Row 0 is the header; row 1 is unit 101 (the note-carrying row),
    // row 2 is unit 102.
    assert_eq!(rows[1][1], "101");
    assert!(rows[1][2].contains("Please update the alternate contact info"));
    assert!(rows[1][2].contains("AlternateContactPhoneNumber"), "note should still carry cell references");
    assert_eq!(rows[2][1], "102");
    assert_eq!(rows[2][2], "", "note is only written on the group's first row");
}

#[test]
fn generate_xlsx_writes_a_related_tenants_section_without_crashing() {
    // No cell references/hyperlink target for this category — just
    // confirms the third section still serializes correctly when the
    // note has no citations to link.
    let all_records = vec![
        TenantRecord { first_last: "johnsmith".into(), unit_number: "A1".into(), phone_number: "5551234".into(), ..Default::default() },
        TenantRecord { first_last: "janedoe".into(), unit_number: "B2".into(), phone_number: "5551234".into(), ..Default::default() },
    ];

    let report = DedupReport {
        total_rows: 2,
        unique_tenants: 2,
        multi_unit_tenants: 0,
        flagged_groups: vec![],
        typo_variant_candidates: vec![],
        related_tenant_candidates: vec![unitprep_dedup::RelatedTenantCandidate {
            group_keys: vec!["janedoe".to_string(), "johnsmith".to_string()],
            signal: unitprep_dedup::RelatednessSignal::SharedPhone,
            shared_value: "5551234".to_string(),
            note: "These tenants share the same phone number (5551234).".to_string(),
        }],
    };

    let xlsx_bytes = generate_xlsx(&report, &all_records).expect("xlsx generation should succeed");
    let rows = read_back(xlsx_bytes);

    let marker_row = rows.iter().find(|row| row[0].contains("Possible related tenants"));
    assert!(marker_row.is_some());

    let note_row = rows.iter().find(|row| row[2].contains("share the same phone number"));
    assert!(note_row.is_some());
}
