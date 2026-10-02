use super::*;
use unitprep_dedup::types::{
    FieldCategory, FieldMismatch, FieldName, FieldValueMismatch, FlaggedGroup,
};

fn record(unit: &str, alt_phone: &str) -> TenantRecord {
    TenantRecord {
        unit_number: unit.to_string(),
        alt_contact_phone_number: alt_phone.to_string(),
        ..Default::default()
    }
}

fn flagged_group(key: &str, records: Vec<TenantRecord>, units: &str) -> FlaggedGroup {
    FlaggedGroup {
        group: TenantGroup {
            key: key.to_string(),
            records,
        },
        mismatches: vec![FieldMismatch {
            category: FieldCategory::AltContact,
            fields: vec![FieldValueMismatch {
                field: FieldName::AltContactPhoneNumber,
                values: vec!["5551234".into(), "(blank)".into()],
            }],
        }],
        note: format!("Please update the alternate contact info to match across units {units}."),
    }
}

fn data_rows(plan: &[PlannedRow]) -> Vec<(&str, &str, usize)> {
    plan.iter()
        .filter_map(|row| match row {
            PlannedRow::Data {
                record,
                note,
                cluster,
                ..
            } => Some((record.unit_number.as_str(), note.as_str(), *cluster)),
            _ => None,
        })
        .collect()
}

#[test]
fn each_group_gets_its_own_increasing_cluster_index() {
    let report = DedupReport {
        flagged_groups: vec![
            flagged_group(
                "smith",
                vec![record("101", ""), record("102", "5551234")],
                "101, 102",
            ),
            flagged_group(
                "jones",
                vec![record("201", "5559876"), record("202", "")],
                "201, 202",
            ),
        ],
        ..Default::default()
    };

    let plan = build_export_plan(&report, &[]);
    let rows = data_rows(&plan);

    assert_eq!(rows[0].2, 0); // unit 101, first group
    assert_eq!(rows[1].2, 0); // unit 102, same group
    assert_eq!(rows[2].2, 1); // unit 201, second group
    assert_eq!(rows[3].2, 1); // unit 202, same group
}

#[test]
fn only_the_first_row_of_a_group_carries_the_note_and_hyperlink_target() {
    let report = DedupReport {
        flagged_groups: vec![flagged_group(
            "smith",
            vec![record("101", ""), record("102", "5551234")],
            "101, 102",
        )],
        ..Default::default()
    };

    let plan = build_export_plan(&report, &[]);
    let data: Vec<_> = plan
        .iter()
        .filter_map(|row| match row {
            PlannedRow::Data {
                note,
                hyperlink_target,
                ..
            } => Some((note.clone(), hyperlink_target.clone())),
            _ => None,
        })
        .collect();

    assert!(!data[0].0.is_empty());
    assert_eq!(data[0].1, Some("T2".to_string())); // AlternateContactPhoneNumber = col T, first row = 2

    assert!(data[1].0.is_empty());
    assert_eq!(data[1].1, None);
}

#[test]
fn blank_rows_separate_groups_and_do_not_break_cluster_counting() {
    let report = DedupReport {
        flagged_groups: vec![
            flagged_group("smith", vec![record("101", "")], "101"),
            flagged_group("jones", vec![record("201", "")], "201"),
        ],
        ..Default::default()
    };

    let plan = build_export_plan(&report, &[]);

    assert!(matches!(plan[0], PlannedRow::Data { .. })); // smith, unit 101
    assert!(matches!(plan[1], PlannedRow::Blank));
    assert!(matches!(plan[2], PlannedRow::Data { .. })); // jones, unit 201
}

#[test]
fn related_tenant_rows_never_get_a_hyperlink_target() {
    let all_records = vec![
        TenantRecord {
            first_last: "johnsmith".into(),
            unit_number: "A1".into(),
            phone_number: "5551234".into(),
            ..Default::default()
        },
        TenantRecord {
            first_last: "janedoe".into(),
            unit_number: "B2".into(),
            phone_number: "5551234".into(),
            ..Default::default()
        },
    ];

    let report = DedupReport {
        related_tenant_candidates: vec![unitprep_dedup::RelatedTenantCandidate {
            group_keys: vec!["janedoe".to_string(), "johnsmith".to_string()],
            evidence: vec![unitprep_dedup::RelatedTenantEvidence {
                signal: unitprep_dedup::RelatednessSignal::SharedPhone,
                shared_value: "5551234".to_string(),
                group_keys: vec!["janedoe".to_string(), "johnsmith".to_string()],
            }],
            note: "shared phone note".to_string(),
        }],
        ..Default::default()
    };

    let plan = build_export_plan(&report, &all_records);

    for row in &plan {
        if let PlannedRow::Data {
            hyperlink_target, ..
        } = row
        {
            assert_eq!(*hyperlink_target, None);
        }
    }
}

#[test]
fn duplicate_customer_records_get_their_own_marked_section_after_flagged_groups() {
    use unitprep_dedup::duplicate_records::{DuplicateCustomerRecord, DuplicateTenantEntry};

    let tenant = |id: &str, unit: &str| DuplicateTenantEntry {
        tenant_id: id.to_string(),
        units: vec![unit.to_string()],
        records: vec![TenantRecord {
            tenant_id: id.to_string(),
            unit_number: unit.to_string(),
            ..Default::default()
        }],
    };
    let report = DedupReport {
        flagged_groups: vec![flagged_group(
            "smith",
            vec![record("A1", "1"), record("A2", "")],
            "A1 and A2",
        )],
        duplicate_customer_records: vec![DuplicateCustomerRecord {
            name_key: "frank flores".to_string(),
            display_name: "Frank Flores".to_string(),
            tenants: vec![tenant("186417", "B18"), tenant("182866", "B26")],
            mismatches: Vec::new(),
            note: "Frank Flores has 2 separate customer records.".to_string(),
        }],
        ..Default::default()
    };

    let plan = build_export_plan(&report, &[]);

    let markers: Vec<&str> = plan
        .iter()
        .filter_map(|row| match row {
            PlannedRow::Marker(text) => Some(*text),
            _ => None,
        })
        .collect();
    assert_eq!(markers.len(), 1);
    assert!(markers[0].contains("duplicate customer records"));

    let rows = data_rows(&plan);
    // Flagged group first (cluster 0), then the finding's two records
    // (cluster 1): the note sits on the first row only.
    assert_eq!(rows[0].2, 0);
    assert_eq!(
        rows[2],
        ("B18", "Frank Flores has 2 separate customer records.", 1)
    );
    assert_eq!(rows[3], ("B26", "", 1));
}

#[test]
fn tenants_without_a_customer_id_get_their_own_marked_section_with_a_note() {
    use unitprep_dedup::unidentified::{IdentifiedMatch, UnidentifiedTenant, UnidentifiedTenants};
    use unitprep_dedup::UnidentifiedMode;

    let named = |id: &str, unit: &str| TenantRecord {
        tenant_id: id.to_string(),
        first_last: "Ann Lee".to_string(),
        unit_number: unit.to_string(),
        ..Default::default()
    };

    let report = DedupReport {
        unidentified: Some(UnidentifiedTenants {
            mode: UnidentifiedMode::MatchedByName,
            tenants: vec![UnidentifiedTenant {
                key: "ann lee".to_string(),
                display_name: "Ann Lee".to_string(),
                units: vec!["9".to_string()],
                same_name_as: vec![IdentifiedMatch {
                    tenant_id: "55".to_string(),
                    display_name: "Ann Lee".to_string(),
                    units: vec!["1".to_string()],
                }],
            }],
            ..Default::default()
        }),
        ..Default::default()
    };

    let plan = build_export_plan(&report, &[named("55", "1"), named("", "9")]);

    assert!(plan.iter().any(|row| matches!(
        row,
        PlannedRow::Marker(text) if text.starts_with("Tenants without a customer ID")
    )));
    let rows = data_rows(&plan);
    assert_eq!(rows.len(), 1, "only the tenant's own id-less row is listed");
    assert_eq!(rows[0].0, "9");
    assert!(rows[0].1.contains("customer ID 55"), "note: {}", rows[0].1);
}
