use super::*;
use unitprep_dedup::types::TenantRecord;

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
