use super::*;
use unitprep_dedup::types::FieldValueMismatch;

fn record(unit: &str, alt_phone: &str) -> TenantRecord {
    TenantRecord { unit_number: unit.to_string(), alt_contact_phone_number: alt_phone.to_string(), ..Default::default() }
}

#[test]
fn col_letter_matches_spreadsheet_convention() {
    assert_eq!(col_letter(0), "A");
    assert_eq!(col_letter(1), "B");
    assert_eq!(col_letter(24), "Y");
    assert_eq!(col_letter(25), "Z");
    assert_eq!(col_letter(26), "AA");
    assert_eq!(col_letter(27), "AB");
    assert_eq!(col_letter(51), "AZ");
    assert_eq!(col_letter(52), "BA");
}

#[test]
fn csv_column_name_covers_every_field_and_stays_in_columns() {
    // AltContact* internally vs. AlternateContact* in the export header is
    // exactly the divergence this mapping exists to bridge — assert both
    // sides explicitly rather than just "some string came back".
    assert_eq!(csv_column_name(FieldName::AltContactPhoneNumber), "AlternateContactPhoneNumber");
    assert_eq!(csv_column_name(FieldName::AddressCity), "AddressCity");
    assert_eq!(csv_column_name(FieldName::FirstName), "FirstName");

    for field in [
        FieldName::PhoneNumber,
        FieldName::PhoneNumberPrefix,
        FieldName::Email,
        FieldName::AddressStreet1,
        FieldName::AddressStreet2,
        FieldName::AddressCity,
        FieldName::AddressState,
        FieldName::AddressPostalCode,
        FieldName::AltContactFirstName,
        FieldName::AltContactLastName,
        FieldName::AltContactEmail,
        FieldName::AltContactPhoneNumber,
        FieldName::AltContactPhoneNumberPrefix,
        FieldName::AltContactAddressStreet1,
        FieldName::AltContactAddressStreet2,
        FieldName::AltContactAddressCity,
        FieldName::AltContactAddressState,
        FieldName::AltContactAddressPostalCode,
        FieldName::CompanyName,
        FieldName::FirstName,
        FieldName::LastName,
    ] {
        let name = csv_column_name(field);
        assert!(COLUMNS.contains(&name), "{name} (from {field:?}) is missing from COLUMNS");
    }
}

#[test]
fn note_with_cell_refs_cites_the_right_cells_for_each_distinct_value() {
    let records = vec![record("S-31", "3605525629"), record("D-216", "3607281619"), record("S-51", "")];

    let note = note_with_cell_refs(
        "Please update the alternate contact info to match across units D-216, S-31, S-51.",
        &records,
        &[FieldName::AltContactPhoneNumber],
        7, // first row these 3 records land on in the final CSV
    );

    // AlternateContactPhoneNumber is column T (index 19) in COLUMNS.
    assert_eq!(
        note,
        "Please update the alternate contact info to match across units D-216, S-31, S-51.  \
         [AlternateContactPhoneNumber: T7=3605525629, T8=3607281619, T9=(blank)]"
    );
}

#[test]
fn note_with_cell_refs_is_a_no_op_without_cite_fields() {
    let records = vec![record("A-1", "555")];
    let note = note_with_cell_refs("Some note.", &records, &[], 2);
    assert_eq!(note, "Some note.");
}

#[test]
fn generate_csv_assigns_row_numbers_matching_actual_output_position() {
    // Two flagged groups: the second group's cell references must land on
    // the rows it's actually written at (after group one's rows *and* the
    // blank separator row between groups), not just "starting from row 2"
    // — this is the exact bug class the reference script's own
    // implementation notes call out (row numbers can't be computed
    // per-group in isolation).
    let group_one = TenantGroup {
        key: "smith".to_string(),
        records: vec![record("101", ""), record("102", "5551234")],
    };
    let group_two = TenantGroup {
        key: "jones".to_string(),
        records: vec![record("201", "5559876"), record("202", "")],
    };

    let report = DedupReport {
        total_rows: 4,
        unique_tenants: 2,
        multi_unit_tenants: 2,
        flagged_groups: vec![
            FlaggedGroup {
                group: group_one,
                mismatches: vec![FieldMismatch {
                    category: FieldCategory::AltContact,
                    fields: vec![FieldValueMismatch {
                        field: FieldName::AltContactPhoneNumber,
                        values: vec!["5551234".into(), "(blank)".into()],
                    }],
                }],
                note: "Please update the alternate contact info to match across units 101, 102."
                    .to_string(),
            },
            FlaggedGroup {
                group: group_two,
                mismatches: vec![FieldMismatch {
                    category: FieldCategory::AltContact,
                    fields: vec![FieldValueMismatch {
                        field: FieldName::AltContactPhoneNumber,
                        values: vec!["5559876".into(), "(blank)".into()],
                    }],
                }],
                note: "Please update the alternate contact info to match across units 201, 202."
                    .to_string(),
            },
        ],
        typo_variant_candidates: vec![],
    };

    let csv_bytes = generate_csv(&report, &[]).expect("csv generation should succeed");
    let csv_text = String::from_utf8(csv_bytes).expect("valid utf-8");

    // Group one occupies rows 2-3 (row 1 is the header): unit 101 is
    // blank, unit 102 has the phone number.
    assert!(csv_text.contains("T2=(blank), T3=5551234"));
    // Row 4 is the blank separator; group two starts at row 5: unit 201
    // has the phone number, unit 202 is blank.
    assert!(csv_text.contains("T5=5559876, T6=(blank)"));
}
