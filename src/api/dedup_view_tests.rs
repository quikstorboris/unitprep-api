use super::*;
use unitprep_dedup::types::{
    FieldCategory, FieldMismatch, FieldValueMismatch, FlaggedGroup, TenantGroup, TypoVariantCandidate,
};

fn record(unit: &str, first: &str, last: &str, email: &str) -> TenantRecord {
    TenantRecord {
        unit_number: unit.to_string(),
        first_last: format!("{first}{last}"),
        first_name: first.to_string(),
        last_name: last.to_string(),
        email: email.to_string(),
        ..Default::default()
    }
}

#[test]
fn flagged_group_gets_display_name_categories_and_cell_refs() {
    let group = TenantGroup {
        key: "smith".to_string(),
        records: vec![
            record("101", "John", "Smith", "a@example.com"),
            record("204", "John", "Smith", ""),
        ],
    };

    let flagged = FlaggedGroup {
        note: "Please update the email address to match across units 101 and 204. Email address \
               is a@example.com for unit 101, but blank for unit 204."
            .to_string(),
        mismatches: vec![FieldMismatch {
            category: FieldCategory::Email,
            fields: vec![FieldValueMismatch {
                field: FieldName::Email,
                values: vec!["a@example.com".into(), "(blank)".into()],
            }],
        }],
        group,
    };

    let report = DedupReport { flagged_groups: vec![flagged], ..Default::default() };
    let view = build_report_view(&report, &[]);

    assert_eq!(view.flagged_groups.len(), 1);
    let group_view = &view.flagged_groups[0];

    assert_eq!(group_view.display_name, "John Smith");
    assert_eq!(group_view.units, vec!["101", "204"]);
    assert_eq!(group_view.categories, vec![FieldCategory::Email]);

    assert_eq!(group_view.bullets.len(), 1);
    let bullet = &group_view.bullets[0];
    assert_eq!(bullet.field, FieldName::Email);
    assert_eq!(bullet.label, "Email address");
    assert_eq!(bullet.sentence, "Email address is a@example.com for unit 101, but blank for unit 204.");
    // Email is column J (index 9); this is the only flagged group, so its
    // two records land on rows 2 and 3 (row 1 is the header).
    assert_eq!(bullet.cell_refs, vec!["J2", "J3"]);
}

#[test]
fn second_flagged_group_cell_refs_account_for_the_blank_separator_row() {
    let group_a = TenantGroup {
        key: "smith".to_string(),
        records: vec![record("101", "John", "Smith", "a@example.com"), record("204", "John", "Smith", "")],
    };
    let group_b = TenantGroup {
        key: "jones".to_string(),
        records: vec![record("301", "Ann", "Jones", "b@example.com"), record("302", "Ann", "Jones", "")],
    };

    let mismatch = |values: Vec<&str>| FieldMismatch {
        category: FieldCategory::Email,
        fields: vec![FieldValueMismatch {
            field: FieldName::Email,
            values: values.into_iter().map(String::from).collect(),
        }],
    };

    let report = DedupReport {
        flagged_groups: vec![
            FlaggedGroup { group: group_a, mismatches: vec![mismatch(vec!["a@example.com", "(blank)"])], note: String::new() },
            FlaggedGroup { group: group_b, mismatches: vec![mismatch(vec!["b@example.com", "(blank)"])], note: String::new() },
        ],
        ..Default::default()
    };

    let view = build_report_view(&report, &[]);

    // Group A: header (row 1), then rows 2-3. Group B: blank separator
    // (row 4), then rows 5-6 — the on-screen cell refs must account for
    // that blank row exactly as the real export would.
    assert_eq!(view.flagged_groups[0].bullets[0].cell_refs, vec!["J2", "J3"]);
    assert_eq!(view.flagged_groups[1].bullets[0].cell_refs, vec!["J5", "J6"]);
}

#[test]
fn typo_variant_resolves_real_display_names_and_units() {
    let records = vec![
        record("101", "Warren", "Carolle", ""),
        record("102", "Warren", "Carolle", ""),
        record("204", "Warren", "Carroll", ""),
    ];

    let report = DedupReport {
        typo_variant_candidates: vec![TypoVariantCandidate {
            key_a: "warrencarolle".to_string(),
            key_b: "warrencarroll".to_string(),
            ratio: 0.95,
            contact_info_matches: true,
            note: "note text".to_string(),
        }],
        ..Default::default()
    };

    let view = build_report_view(&report, &records);

    assert_eq!(view.typo_variant_candidates.len(), 1);
    let variant = &view.typo_variant_candidates[0];

    assert_eq!(variant.display_name_a, "Warren Carolle");
    assert_eq!(variant.units_a, vec!["101", "102"]);
    assert_eq!(variant.display_name_b, "Warren Carroll");
    assert_eq!(variant.units_b, vec!["204"]);
}
