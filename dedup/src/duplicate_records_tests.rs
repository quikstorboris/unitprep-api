use super::*;
use crate::grouping::group_records_by_tenant;
use crate::report::run;

fn rec(tenant_id: &str, name: &str, unit: &str) -> TenantRecord {
    let (first, last) = name.split_once(' ').unwrap_or((name, ""));
    TenantRecord {
        tenant_id: tenant_id.to_string(),
        first_last: name.to_lowercase(),
        first_name: first.to_string(),
        last_name: last.to_string(),
        unit_number: unit.to_string(),
        phone_number: "(575) 555-0100".to_string(),
        email: "f@example.com".to_string(),
        address_street1: "1 Main St".to_string(),
        address_city: "Alamogordo".to_string(),
        address_state: "NM".to_string(),
        address_postal_code: "88310".to_string(),
        ..Default::default()
    }
}

/// The LG Squared validation shape: one person, two customer ids, each on
/// its own unit, with identical contact details.
fn flores() -> Vec<TenantRecord> {
    vec![
        rec("186417", "Frank Flores", "B18"),
        rec("182866", "Frank Flores", "B26"),
    ]
}

#[test]
fn one_name_under_two_tenant_ids_is_reported_even_though_the_contact_details_match() {
    let report = run(flores());

    assert_eq!(report.unique_tenants, 2);
    assert_eq!(report.multi_unit_tenants, 0);
    assert!(report.flagged_groups.is_empty());
    assert_eq!(report.duplicate_customer_records.len(), 1);

    let finding = &report.duplicate_customer_records[0];
    assert_eq!(finding.display_name, "Frank Flores");
    assert!(finding.mismatches.is_empty());
    let ids: Vec<&str> = finding
        .tenants
        .iter()
        .map(|t| t.tenant_id.as_str())
        .collect();
    assert_eq!(ids, vec!["186417", "182866"]);
    assert_eq!(finding.tenants[0].units, vec!["B18"]);
    assert_eq!(finding.tenants[1].units, vec!["B26"]);
    assert_eq!(
        finding.note,
        "Frank Flores has 2 separate customer records: ID 186417 (unit B18) and ID 182866 (unit B26). \
         The contact details match, so these can be merged into one customer record."
    );
}

#[test]
fn a_tenant_holding_several_units_under_one_id_is_one_tenant_not_a_duplicate() {
    let records = vec![
        rec("10", "Alice Able", "1"),
        rec("10", "Alice Able", "2"),
        rec("11", "Bob Baker", "3"),
    ];

    let report = run(records);

    assert_eq!(report.unique_tenants, 2);
    assert_eq!(report.multi_unit_tenants, 1);
    assert!(report.duplicate_customer_records.is_empty());
}

#[test]
fn the_validation_scenario_counts_one_tenant_per_id() {
    // 240 rows shaped like the LG Squared Directory, scaled down:
    // Alice holds units 1-2 under one id and a stray third unit under a
    // second id; Bob holds one unit. By id that is 3 tenants, 1 of them
    // multi-unit; by name it was 2 tenants.
    let records = vec![
        rec("10", "Alice Able", "1"),
        rec("10", "Alice Able", "2"),
        rec("12", "Alice Able", "3"),
        rec("11", "Bob Baker", "4"),
    ];

    let report = run(records);

    assert_eq!(report.unique_tenants, 3);
    assert_eq!(report.multi_unit_tenants, 1);
    assert!(report.flagged_groups.is_empty());
    assert_eq!(report.duplicate_customer_records.len(), 1);
    assert_eq!(
        report.duplicate_customer_records[0].tenants[0].units,
        vec!["1", "2"]
    );
    assert_eq!(
        report.duplicate_customer_records[0].tenants[0]
            .records
            .len(),
        2
    );
}

#[test]
fn contact_differences_between_the_duplicate_records_travel_with_the_finding() {
    let mut records = flores();
    records[1].phone_number = "(575) 555-0199".to_string();

    let report = run(records);

    // No longer a flagged group (the records belong to different
    // tenants), but the disagreement is not lost.
    assert!(report.flagged_groups.is_empty());
    let finding = &report.duplicate_customer_records[0];
    let categories: Vec<FieldCategory> = finding.mismatches.iter().map(|m| m.category).collect();
    assert_eq!(categories, vec![FieldCategory::Phone]);
    assert!(
        finding
            .note
            .contains("The contact details also differ (phone number)"),
        "{}",
        finding.note
    );
}

#[test]
fn a_real_contact_mismatch_inside_one_tenant_id_is_still_flagged() {
    let mut records = vec![rec("10", "Alice Able", "1"), rec("10", "Alice Able", "2")];
    records[1].email = "other@example.com".to_string();

    let report = run(records);

    assert_eq!(report.flagged_groups.len(), 1);
    assert!(report.duplicate_customer_records.is_empty());
}

#[test]
fn records_without_a_tenant_id_group_by_name_exactly_as_before() {
    // QSX / Easy Storage Solutions carry no tenant id: nothing changes and
    // there is nothing for the duplicate-record pass to compare.
    let records = vec![
        rec("", "Alice Able", "1"),
        rec("", "Alice Able", "2"),
        rec("", "Bob Baker", "3"),
    ];

    let report = run(records);

    assert_eq!(report.unique_tenants, 2);
    assert_eq!(report.multi_unit_tenants, 1);
    assert!(report.duplicate_customer_records.is_empty());
}

#[test]
fn three_ids_behind_one_name_are_one_finding_listing_all_three() {
    let records = vec![
        rec("1", "Tom Barrett", "A"),
        rec("2", "Tom Barrett", "B"),
        rec("3", "Tom Barrett", "C"),
    ];

    let report = run(records);

    assert_eq!(report.duplicate_customer_records.len(), 1);
    assert_eq!(report.duplicate_customer_records[0].tenants.len(), 3);
    assert!(report.duplicate_customer_records[0]
        .note
        .starts_with("Tom Barrett has 3 separate customer records: ID 1 (unit A), ID 2 (unit B), and ID 3 (unit C)."));
}

#[test]
fn blank_names_are_never_pooled_into_a_duplicate() {
    let mut a = rec("1", "x", "A");
    let mut b = rec("2", "x", "B");
    a.first_last = String::new();
    b.first_last = String::new();

    assert!(find_duplicate_customer_records(&group_records_by_tenant(vec![a, b])).is_empty());
}

#[test]
fn related_tenant_detection_does_not_pair_a_person_with_themselves() {
    // Two ids, one name, one address: the name-keyed view the related
    // pass runs on still sees ONE person, so it must not report "two
    // different names sharing an address".
    let report = run(flores());

    assert!(report.related_tenant_candidates.is_empty());
    assert!(report.typo_variant_candidates.is_empty());
}

#[test]
fn a_unit_label_that_only_repeats_the_tenant_id_is_left_out_of_the_note() {
    // A format with no real unit column maps the id into the unit slot;
    // listing "ID 7 (units 7 and 7)" would be noise.
    let records = vec![
        rec("7", "Ann Lee", "7"),
        rec("7", "Ann Lee", "7"),
        rec("8", "Ann Lee", "8"),
    ];

    let report = run(records);

    let finding = &report.duplicate_customer_records[0];
    assert!(finding.tenants.iter().all(|t| t.units.is_empty()));
    assert!(
        finding
            .note
            .starts_with("Ann Lee has 2 separate customer records: ID 7 and ID 8."),
        "{}",
        finding.note
    );
}

#[test]
fn repeated_rows_for_one_unit_are_listed_once() {
    let records = vec![
        rec("7", "Ann Lee", "A1"),
        rec("7", "Ann Lee", "A1"),
        rec("8", "Ann Lee", "B2"),
    ];

    let report = run(records);

    assert_eq!(
        report.duplicate_customer_records[0].tenants[0].units,
        vec!["A1"]
    );
}
