use super::*;
use unitprep_dedup::types::{FieldCategory, FieldMismatch, FieldName, FieldValueMismatch};
use unitprep_dedup::RelatednessSignal;

fn record(unit: &str, alt_phone: &str) -> TenantRecord {
    TenantRecord { unit_number: unit.to_string(), alt_contact_phone_number: alt_phone.to_string(), ..Default::default() }
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
        related_tenant_candidates: vec![],
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

#[test]
fn generate_csv_writes_a_related_tenants_section() {
    let all_records = vec![
        TenantRecord {
            first_last: "johnsmith".to_string(),
            unit_number: "A1".to_string(),
            phone_number: "5551234".to_string(),
            ..Default::default()
        },
        TenantRecord {
            first_last: "janedoe".to_string(),
            unit_number: "B2".to_string(),
            phone_number: "5551234".to_string(),
            ..Default::default()
        },
    ];

    let report = DedupReport {
        total_rows: 2,
        unique_tenants: 2,
        multi_unit_tenants: 0,
        flagged_groups: vec![],
        typo_variant_candidates: vec![],
        related_tenant_candidates: vec![RelatedTenantCandidate {
            group_keys: vec!["janedoe".to_string(), "johnsmith".to_string()],
            signal: RelatednessSignal::SharedPhone,
            shared_value: "5551234".to_string(),
            note: "These tenants share the same phone number (5551234) despite having different \
                   names — worth checking whether these are related tenants."
                .to_string(),
        }],
    };

    let csv_bytes = generate_csv(&report, &all_records).expect("csv generation should succeed");
    let csv_text = String::from_utf8(csv_bytes).expect("valid utf-8");

    assert!(csv_text.contains("Possible related tenants (shared contact info, different names)"));
    assert!(csv_text.contains("share the same phone number (5551234)"));
    assert!(csv_text.contains("A1"));
    assert!(csv_text.contains("B2"));
}
