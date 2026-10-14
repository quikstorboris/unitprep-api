use super::*;

fn r(cells: &[(usize, &str)]) -> Vec<String> {
    let mut row = vec![String::new(); 26];
    for (c, v) in cells {
        row[*c] = (*v).to_string();
    }
    row
}

fn title_rows() -> Vec<Vec<String>> {
    vec![
        r(&[(1, "Tenant List - All Tenants")]),
        r(&[(1, "Some Storage, 1 Main St, Town, OR 97000")]),
        r(&[]),
        r(&[(10, "Created: 10/14/2026, 10:49:54AM")]),
        r(&[]),
    ]
}

fn unit_header() -> Vec<String> {
    r(&[
        (5, "Unit #"),
        (9, "Unit Type"),
        (13, "Rate"),
        (17, "Move In"),
        (21, "Next Billing"),
        (25, "Overlock"),
    ])
}

/// Betty: two phones, two units, a comment beside the first unit line.
fn betty() -> Vec<Vec<String>> {
    vec![
        r(&[
            (1, "Kreger, Betty"),
            (5, "[H]"),
            (7, "305-304-8615"),
            (12, "Balance:"),
            (15, "-180.0"),
            (19, "Access #:"),
            (24, "1234"),
        ]),
        r(&[]),
        r(&[
            (5, "[C]"),
            (7, "305-555-0100"),
            (12, "Deposit:"),
            (15, "50.0"),
        ]),
        r(&[(1, "274 16th ST APT 7")]),
        r(&[(12, "Email:"), (15, "betty@x.com")]),
        r(&[]),
        r(&[(1, "REEDSPORT, OR 97467")]),
        r(&[]),
        unit_header(),
        r(&[
            (5, "0043"),
            (9, "10 x 15"),
            (13, "165.0"),
            (17, "45296"),
            (21, "46331"),
        ]),
        r(&[(1, "Comments:")]),
        r(&[
            (1, "Authorized: Bob"),
            (5, "0017"),
            (9, "10 x 20"),
            (13, "195.0"),
            (17, "45548"),
            (21, "46331"),
            (25, "X"),
        ]),
        r(&[]),
    ]
}

/// A former tenant: a full block with no unit line.
fn former_tenant() -> Vec<Vec<String>> {
    vec![
        r(&[
            (1, "Gone, Gary"),
            (5, "[H]"),
            (7, "111"),
            (12, "Balance:"),
            (15, "0.0"),
        ]),
        r(&[(1, "1 Old Rd")]),
        unit_header(),
        r(&[(1, "Comments:")]),
        r(&[]),
    ]
}

fn sheet(blocks: &[Vec<Vec<String>>]) -> Vec<Vec<String>> {
    let mut rows = title_rows();
    for block in blocks {
        rows.extend(block.iter().cloned());
    }
    rows
}

fn col(report: &FlattenedReport, name: &str) -> usize {
    report
        .headers
        .iter()
        .position(|h| h == name)
        .unwrap_or_else(|| panic!("no column {name}"))
}

#[test]
fn recognizes_the_tenant_list_by_its_title_and_created_stamp() {
    assert!(is_tenant_list(&sheet(&[betty()])));
    assert!(!is_tenant_list(&[
        r(&[(1, "Rent Roll")]),
        r(&[(1, "Created: x")])
    ]));
    assert!(!is_tenant_list(&[r(&[(0, "Unit"), (1, "Tenant")])]));
}

#[test]
fn a_record_with_two_units_becomes_two_rows_sharing_contact_and_record_id() {
    let report = flatten_tenant_list(&sheet(&[betty()])).expect("one tenant with units");

    assert_eq!(report.rows.len(), 2);
    for row in &report.rows {
        assert_eq!(row[col(&report, "Tenant")], "Kreger, Betty");
        assert_eq!(row[col(&report, "Tenant Record ID")], "R-0043");
        assert_eq!(row[col(&report, "Phone Type 1")], "[H]");
        assert_eq!(row[col(&report, "Phone 1")], "305-304-8615");
        assert_eq!(row[col(&report, "Phone Type 2")], "[C]");
        assert_eq!(row[col(&report, "Phone 2")], "305-555-0100");
        assert_eq!(row[col(&report, "Address Line 1")], "274 16th ST APT 7");
        assert_eq!(row[col(&report, "Address Line 2")], "REEDSPORT, OR 97467");
        assert_eq!(row[col(&report, "Email")], "betty@x.com");
        assert_eq!(row[col(&report, "Access #")], "1234");
        assert_eq!(row[col(&report, "Balance")], "-180.0");
        assert_eq!(row[col(&report, "Deposit")], "50.0");
        assert_eq!(row[col(&report, "Comments")], "Authorized: Bob");
    }
    assert_eq!(report.rows[0][col(&report, "Unit #")], "0043");
    assert_eq!(report.rows[0][col(&report, "Unit Type")], "10 x 15");
    assert_eq!(report.rows[0][col(&report, "Rate")], "165.0");
    assert_eq!(report.rows[0][col(&report, "Overlock")], "");
    assert_eq!(report.rows[1][col(&report, "Unit #")], "0017");
    assert_eq!(report.rows[1][col(&report, "Overlock")], "X");
}

#[test]
fn former_tenants_without_a_unit_line_are_left_out() {
    let report = flatten_tenant_list(&sheet(&[former_tenant(), betty()])).expect("betty has units");

    assert_eq!(report.rows.len(), 2);
    assert!(report
        .rows
        .iter()
        .all(|row| row[col(&report, "Tenant")] == "Kreger, Betty"));
}

#[test]
fn two_records_under_one_name_keep_their_own_record_ids() {
    let mut second = betty();
    second[9][5] = "0050".to_string();
    second[11][5] = "0051".to_string();

    let report = flatten_tenant_list(&sheet(&[betty(), second])).expect("two records");

    let ids: Vec<&str> = report
        .rows
        .iter()
        .map(|row| row[col(&report, "Tenant Record ID")].as_str())
        .collect();
    assert_eq!(ids, vec!["R-0043", "R-0043", "R-0050", "R-0050"]);
}

#[test]
fn a_phone_on_the_email_row_is_still_a_phone_not_an_email() {
    let mut block = betty();
    // Third phone sits on the Email row, left of the label.
    block[4][5] = "[B]".to_string();
    block[4][7] = "999-111-2222 shop".to_string();

    let report = flatten_tenant_list(&sheet(&[block])).unwrap();

    assert_eq!(report.rows[0][col(&report, "Phone Type 3")], "[B]");
    assert_eq!(report.rows[0][col(&report, "Phone 3")], "999-111-2222 shop");
    assert_eq!(report.rows[0][col(&report, "Email")], "betty@x.com");
}

#[test]
fn a_block_with_no_street_keeps_its_single_address_line() {
    let mut block = betty();
    block.remove(3); // the street line
    let report = flatten_tenant_list(&sheet(&[block])).unwrap();

    assert_eq!(
        report.rows[0][col(&report, "Address Line 1")],
        "REEDSPORT, OR 97467"
    );
    assert_eq!(report.rows[0][col(&report, "Address Line 2")], "");
}

#[test]
fn blocks_from_a_second_sheet_flatten_with_the_first() {
    // The caller concatenates every sheet; the second one has no title.
    let mut rows = sheet(&[betty()]);
    let mut other = betty();
    other[0][1] = "Other, Olive".to_string();
    other[9][5] = "0200".to_string();
    other[11][5] = "0201".to_string();
    rows.extend(other);

    let report = flatten_tenant_list(&rows).unwrap();

    assert_eq!(report.rows.len(), 4);
    assert_eq!(report.rows[2][col(&report, "Tenant")], "Other, Olive");
}

#[test]
fn a_sheet_of_only_former_tenants_is_not_a_report_with_rows() {
    assert!(flatten_tenant_list(&sheet(&[former_tenant()])).is_none());
}
