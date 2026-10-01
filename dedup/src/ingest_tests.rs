use super::*;
use unitprep_core::vendor_format::ContentType;

fn document(headers: Vec<&str>, rows: Vec<Vec<&str>>) -> CsvDocument {
    CsvDocument {
        file_name: "test.csv".to_string(),
        headers: headers.into_iter().map(String::from).collect(),
        rows: rows
            .into_iter()
            .map(|row| row.into_iter().map(String::from).collect())
            .collect(),
        modified_at: None,
    }
}

/// QSX's real signature/mapping, hand-built to mirror the
/// `client_ops.vendor_format` registry migration's seed row for
/// `content_type = 'tenants'` — QSX's own headers already equal the
/// canonical names, so its mapping is a pure identity.
fn qsx_vendor() -> VendorFormat {
    let columns = [
        "CustNumb",
        "UnitNumber",
        "FirtLast",
        "FirstName",
        "LastName",
        "CompanyName",
        "PhoneNumber",
        "Email",
        "AddressStreet1",
    ];

    VendorFormat {
        name: "QSX".to_string(),
        content_type: ContentType::Tenants,
        signature_headers: vec![
            "FirtLast".to_string(),
            "CustNumb".to_string(),
            "AddressStreet1".to_string(),
        ],
        field_mapping: columns
            .iter()
            .map(|c| (c.to_string(), c.to_string()))
            .collect(),
        transform_key: None,
    }
}

fn qsx_vendors() -> Vec<VendorFormat> {
    vec![qsx_vendor()]
}

/// Easy Storage Solutions' real signature/mapping, mirroring the
/// `client_ops.vendor_format` registry migration's seed row exactly
/// (name, signature, field_mapping, and `transform_key` all copied
/// from there) — onboarded from a real Louisiana facility's
/// "Full Tenant Data.csv" export.
fn ess_vendor() -> VendorFormat {
    let mapping = [
        ("UnitNumber", "Unit"),
        ("FirtLast", "Name"),
        ("AlternateContactFirstName", "Alternate Contact"),
        ("PhoneNumber", "Phone"),
        ("Email", "Email"),
        ("AlternateContactPhoneNumber", "Alternate Phone"),
        ("AlternateContactAddressStreet1", "Alternate Address"),
        ("AlternateContactAddressCity", "Alternate City"),
        ("AlternateContactAddressState", "Alternate State"),
        ("AlternateContactAddressPostalCode", "Alternate Zip"),
        ("AddressStreet1", "AddressStreet1"),
        ("AddressCity", "AddressCity"),
        ("AddressState", "AddressState"),
        ("AddressPostalCode", "AddressPostalCode"),
    ];

    VendorFormat {
        name: "Easy Storage Solutions".to_string(),
        content_type: ContentType::Tenants,
        signature_headers: vec![
            "Unit".to_string(),
            "Move-in Date".to_string(),
            "Tenant Protection".to_string(),
        ],
        field_mapping: mapping
            .iter()
            .map(|(t, s)| (t.to_string(), s.to_string()))
            .collect(),
        transform_key: Some("split_ess_address".to_string()),
    }
}

#[test]
fn builds_a_tenant_record_from_a_matching_row() {
    let doc = document(
        vec![
            "CustNumb",
            "UnitNumber",
            "FirtLast",
            "Email",
            "AddressStreet1",
        ],
        vec![vec![
            "C1",
            "101",
            "Doe, Jane",
            "jane@example.com",
            "1 Main St",
        ]],
    );

    let records = records_from_csv_document(&doc, &qsx_vendors()).expect("known-good QSX document");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].cust_numb, "C1");
    assert_eq!(records[0].unit_number, "101");
    assert_eq!(records[0].first_last, "Doe, Jane");
    assert_eq!(records[0].email, "jane@example.com");
    // Every column this crate doesn't recognize/wasn't present is left
    // at TenantRecord::default() rather than erroring -- same
    // tolerance the reference script has via dict.get(field, "").
    assert_eq!(records[0].company_name, "");
}

/// End-to-end: a real Easy Storage Solutions row (from the actual
/// Louisiana "Full Tenant Data.csv" export this vendor was added
/// for) gets detected, its combined `Address` column split via the
/// `split_ess_address` transform, and built into a `TenantRecord`
/// with the same grouping key (`FirtLast`) and address fields QSX
/// rows carry — the whole point of normalizing before this crate's
/// own extraction ever runs.
#[test]
fn detects_and_normalizes_a_real_ess_style_row() {
    let doc = document(
        vec![
            "Unit",
            "Unit Type",
            "Move-in Date",
            "Billing Date",
            "Name",
            "Address",
            "Phone",
            "Cell Phone",
            "Email",
            "Tenant Protection",
            "Alternate Contact",
            "Alternate Phone",
            "Alternate Address",
            "Alternate City",
            "Alternate State",
            "Alternate Zip",
        ],
        vec![vec![
            "1",
            "10x10 Non-Climate Controlled (10 x 10 x 8)",
            "5/7/2026",
            "1st",
            "Lexie Rodrigue",
            "208 Laurel Oak Dr.\nSt. Rose, Louisiana 70087",
            "(504) 908-5239",
            "(504) 908-5239",
            "lexiejrodrigue711@gmail.com",
            "",
            "Jessie Rodrigue",
            "+15046287758",
            "208 Laurel Oak Dr.",
            "St. Rose",
            "Louisiana",
            "70087",
        ]],
    );

    let vendors = vec![ess_vendor()];
    let records = records_from_csv_document(&doc, &vendors).expect("known-good ESS document");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].unit_number, "1");
    assert_eq!(records[0].first_last, "Lexie Rodrigue");
    assert_eq!(records[0].phone_number, "(504) 908-5239");
    assert_eq!(records[0].email, "lexiejrodrigue711@gmail.com");
    assert_eq!(records[0].address_street1, "208 Laurel Oak Dr.");
    assert_eq!(records[0].address_city, "St. Rose");
    assert_eq!(records[0].address_state, "Louisiana");
    assert_eq!(records[0].address_postal_code, "70087");
    assert_eq!(records[0].alt_contact_first_name, "Jessie Rodrigue");
    assert_eq!(records[0].alt_contact_phone_number, "+15046287758");
}

/// QuikStor Cloud's real signature/mapping, mirroring the
/// `client_ops.vendor_format` registry migration's seed row exactly
/// (name, signature, field_mapping, and `transform_key` all copied
/// from `20261001120000_seed_quikstor_cloud_tenants_vendor_format`)
/// -- onboarded from a real facility's preliminary-pull
/// "Tenants.csv".
fn quikstor_cloud_vendor() -> VendorFormat {
    let mapping = [
        ("CustNumb", "LegacyTenantId"),
        ("UnitNumber", "LegacyTenantId"),
        ("TenantId", "LegacyTenantId"),
        ("FirtLast", "FirtLast"),
        ("FirstName", "FirstName"),
        ("LastName", "LastName"),
        ("CompanyName", "CompanyName"),
        ("PhoneNumber", "PhoneNumber"),
        ("PhoneNumberPrefix", "PhoneNumberPrefix"),
        ("Email", "Email"),
        ("AddressStreet1", "AddressLine"),
        ("AddressStreet2", "AddressLineOptional"),
        ("AddressCity", "City"),
        ("AddressState", "State"),
        ("AddressPostalCode", "PostalCode"),
    ];

    VendorFormat {
        name: "QuikStor Cloud".to_string(),
        content_type: ContentType::Tenants,
        signature_headers: [
            "LegacyTenantId",
            "AccountType",
            "FirstName",
            "LastName",
            "AddressLine",
            "CellPhoneNumber",
        ]
        .iter()
        .map(|h| h.to_string())
        .collect(),
        field_mapping: mapping
            .iter()
            .map(|(t, s)| (t.to_string(), s.to_string()))
            .collect(),
        transform_key: Some("derive_quikstor_cloud_tenant_fields".to_string()),
    }
}

fn quikstor_cloud_document(rows: Vec<Vec<&str>>) -> CsvDocument {
    document(
        vec![
            "Email",
            "FirstName",
            "MiddleName",
            "LastName",
            "CompanyName",
            "AccountType",
            "CellPhoneNumberPrefix",
            "CellPhoneNumber",
            "HomePhoneNumberPrefix",
            "HomePhoneNumber",
            "WorkPhoneNumberPrefix",
            "WorkPhoneNumber",
            "LegacyTenantId",
            "AddressLine",
            "AddressLineOptional",
            "City",
            "State",
            "PostalCode",
        ],
        rows,
    )
}

/// End-to-end on rows shaped like a real QuikStor Cloud "Tenants.csv"
/// (synthetic values): no name key, unit column, or single phone
/// column in the source, so recognition plus the transform have to
/// supply all of them.
#[test]
fn detects_and_normalizes_a_quikstor_cloud_row() {
    let doc = quikstor_cloud_document(vec![vec![
        "#NoEmail",
        "Jerry ",
        "Gene",
        "Morrison",
        "Acme Surveying",
        "Individual",
        "",
        "",
        "+1",
        "3606326932",
        "",
        "",
        "1010338",
        "PO BOX 1011",
        "Suite 2",
        "Freeland",
        "Washington",
        "98249",
    ]]);

    let records = records_from_csv_document(&doc, &[quikstor_cloud_vendor()])
        .expect("known-good QuikStor Cloud document");

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].first_last, "Jerry Morrison");
    assert_eq!(records[0].cust_numb, "1010338");
    assert_eq!(records[0].unit_number, "1010338");
    assert_eq!(records[0].phone_number, "3606326932");
    assert_eq!(records[0].phone_number_prefix, "+1");
    assert_eq!(records[0].email, "", "#NoEmail sentinel must read as blank");
    assert_eq!(records[0].company_name, "Acme Surveying");
    assert_eq!(records[0].address_street1, "PO BOX 1011");
    assert_eq!(records[0].address_street2, "Suite 2");
    assert_eq!(records[0].address_postal_code, "98249");
}

/// The point of onboarding this vendor: one person under several
/// tenant IDs with a mistyped phone is flagged, and the note names
/// the records by tenant ID rather than printing blank units.
#[test]
fn quikstor_cloud_rows_with_two_tenant_ids_become_a_duplicate_customer_record() {
    let doc = quikstor_cloud_document(vec![
        vec![
            "tom@example.com",
            "Tom",
            "",
            "Barrett",
            "",
            "Individual",
            "+1",
            "2067786611",
            "",
            "",
            "",
            "",
            "1034224",
            "2038 9th st west",
            "",
            "Kirkland",
            "Washington",
            "98033",
        ],
        vec![
            "tom@example.com",
            "Tom",
            "",
            "Barrett",
            "",
            "Individual",
            "+1",
            "2067796611",
            "",
            "",
            "",
            "",
            "1034223",
            "2038 9th st west",
            "",
            "Kirkland",
            "Washington",
            "98033",
        ],
    ]);

    let records = records_from_csv_document(&doc, &[quikstor_cloud_vendor()])
        .expect("known-good QuikStor Cloud document");
    let report = crate::report::run(records);

    // Two LegacyTenantIds are two customer records, not one tenant: the
    // phone disagreement now travels with the duplicate-record finding.
    assert!(report.flagged_groups.is_empty());
    assert_eq!(report.duplicate_customer_records.len(), 1);
    let finding = &report.duplicate_customer_records[0];
    assert_eq!(finding.display_name, "Tom Barrett");
    assert_eq!(finding.tenants.len(), 2);
    assert!(finding.note.contains("phone number"), "{}", finding.note);
}

#[test]
fn refuses_a_document_that_matches_no_known_vendor() {
    let doc = document(vec!["CustNumb", "UnitNumber"], vec![vec!["C1", "101"]]);

    let err = records_from_csv_document(&doc, &qsx_vendors())
        .expect_err("headers don't satisfy any registered vendor's signature");

    assert!(err
        .to_string()
        .contains("Unrecognized tenant export format"));
}

/// SiteLink's real signature/mapping, mirroring the
/// `client_ops.vendor_format` registry migration's seed row exactly
/// (`20261001130000_seed_sitelink_tenants_vendor_format`) -- onboarded
/// from a real facility's Directory / Rent Roll reports.
fn sitelink_vendor() -> VendorFormat {
    let mapping = [
        ("CustNumb", "LedgerID"),
        ("UnitNumber", "sUnitName"),
        ("TenantId", "TenantID"),
        ("FirtLast", "FirtLast"),
        ("FirstName", "sFName"),
        ("LastName", "sLName"),
        ("CompanyName", "sCompany"),
        ("PhoneNumber", "PhoneNumber"),
        ("Email", "sEmail"),
        ("AddressStreet1", "sAddr1"),
        ("AddressStreet2", "sAddr2"),
        ("AddressCity", "sCity"),
        ("AddressState", "sRegion"),
        ("AddressPostalCode", "sPostalCode"),
        ("AlternateContactFirstName", "sFNameAlt"),
        ("AlternateContactLastName", "sLNameAlt"),
        ("AlternateContactEmail", "sEmailAlt"),
        ("AlternateContactPhoneNumber", "sPhoneAlt"),
        ("AlternateContactAddressStreet1", "sAddr1Alt"),
        ("AlternateContactAddressStreet2", "sAddr2Alt"),
        ("AlternateContactAddressCity", "sCityAlt"),
        ("AlternateContactAddressState", "sRegionAlt"),
        ("AlternateContactAddressPostalCode", "sPostalCodeAlt"),
    ];

    VendorFormat {
        name: "SiteLink".to_string(),
        content_type: ContentType::Tenants,
        signature_headers: [
            "sUnitName",
            "LedgerID",
            "TenantID",
            "sFName",
            "sLName",
            "sAddr1",
            "sEmail",
        ]
        .iter()
        .map(|h| h.to_string())
        .collect(),
        field_mapping: mapping
            .iter()
            .map(|(t, s)| (t.to_string(), s.to_string()))
            .collect(),
        transform_key: Some("derive_sitelink_tenant_fields".to_string()),
    }
}

/// A Rent Roll-shaped document: the signature columns plus the ones the
/// mapping reads. Rent Roll lists vacant units too, as rows with no
/// ledger -- the Directory report is the same minus those rows.
fn sitelink_document(rows: Vec<Vec<&str>>) -> CsvDocument {
    document(
        vec![
            "sUnitName",
            "LedgerID",
            "TenantID",
            "sFName",
            "sLName",
            "sCompany",
            "sPhone",
            "sMobile",
            "sEmail",
            "sAddr1",
            "sCity",
            "sRegion",
            "sPostalCode",
            "sFNameAlt",
            "sLNameAlt",
            "sPhoneAlt",
        ],
        rows,
    )
}

#[test]
fn detects_and_normalizes_a_sitelink_row_and_skips_vacant_units() {
    let doc = sitelink_document(vec![
        vec![
            "A03",
            "239137",
            "136140",
            "Dave",
            "Gonzalez",
            "MDC",
            "",
            "(575) 446-2730",
            "d@example.com",
            "2481 Sedona Ridge",
            "Alamogordo",
            "NM",
            "88310",
            "Leslie",
            "Bryant",
            "515-323-4858",
        ],
        vec![
            "V01", "", "", "", "", "", "", "", "", "", "", "", "", "", "", "",
        ],
    ]);

    let records = records_from_csv_document(&doc, &[sitelink_vendor()])
        .expect("known-good SiteLink document");

    assert_eq!(records.len(), 1, "the vacant unit must not become a record");
    assert_eq!(records[0].first_last, "Dave Gonzalez");
    assert_eq!(records[0].cust_numb, "239137");
    assert_eq!(records[0].unit_number, "A03");
    assert_eq!(
        records[0].phone_number, "(575) 446-2730",
        "falls back to mobile"
    );
    assert_eq!(records[0].company_name, "MDC");
    assert_eq!(records[0].address_state, "NM");
    assert_eq!(records[0].alt_contact_first_name, "Leslie");
    assert_eq!(records[0].alt_contact_phone_number, "515-323-4858");
}

/// SiteLink keeps contact on the tenant record, so the realistic problem
/// is one person entered twice under different tenant IDs. That is a
/// duplicate customer record, reported with the real unit numbers; any
/// contact disagreement between the two records travels with it.
#[test]
fn sitelink_rows_with_two_tenant_ids_become_a_duplicate_customer_record() {
    let doc = sitelink_document(vec![
        vec![
            "B18",
            "1",
            "182866",
            "Frank",
            "Flores",
            "",
            "(575) 111-1111",
            "",
            "f@example.com",
            "1 Main St",
            "Alamogordo",
            "NM",
            "88310",
            "",
            "",
            "",
        ],
        vec![
            "B26",
            "2",
            "186417",
            "Frank",
            "Flores",
            "",
            "(575) 222-2222",
            "",
            "f@example.com",
            "1 Main St",
            "Alamogordo",
            "NM",
            "88310",
            "",
            "",
            "",
        ],
    ]);

    let records = records_from_csv_document(&doc, &[sitelink_vendor()])
        .expect("known-good SiteLink document");
    let report = crate::report::run(records);

    assert!(report.flagged_groups.is_empty());
    assert_eq!(report.duplicate_customer_records.len(), 1);
    let note = &report.duplicate_customer_records[0].note;
    assert!(note.contains("ID 182866 (unit B18)"), "{note}");
    assert!(note.contains("ID 186417 (unit B26)"), "{note}");
    assert!(note.contains("phone number"), "{note}");
}

/// The LG Squared RV & Ministorage validation case: Frank Flores holds
/// units B18 and B26 under TenantIDs 182866 and 186417 with identical
/// address, phone and email. Grouped by name that was one tenant and
/// nothing was reported; by tenant id it is two customer records.
#[test]
fn a_person_with_identical_contact_under_two_tenant_ids_is_still_surfaced() {
    let row = |unit: &'static str, ledger: &'static str, tenant: &'static str| {
        vec![
            unit,
            ledger,
            tenant,
            "Frank",
            "Flores",
            "",
            "(575) 111-1111",
            "",
            "f@example.com",
            "1 Main St",
            "Alamogordo",
            "NM",
            "88310",
            "",
            "",
            "",
        ]
    };
    let doc = sitelink_document(vec![
        row("B18", "1", "182866"),
        row("B26", "2", "186417"),
        // An unrelated tenant holding two units under one id: one multi-unit tenant.
        vec![
            "C01",
            "3",
            "300",
            "Ada",
            "Lovelace",
            "",
            "(575) 333-3333",
            "",
            "a@example.com",
            "2 Oak St",
            "Alamogordo",
            "NM",
            "88310",
            "",
            "",
            "",
        ],
        vec![
            "C02",
            "4",
            "300",
            "Ada",
            "Lovelace",
            "",
            "(575) 333-3333",
            "",
            "a@example.com",
            "2 Oak St",
            "Alamogordo",
            "NM",
            "88310",
            "",
            "",
            "",
        ],
    ]);

    let records = records_from_csv_document(&doc, &[sitelink_vendor()])
        .expect("known-good SiteLink document");
    let report = crate::report::run(records);

    assert_eq!(report.unique_tenants, 3, "Flores counts once per tenant id");
    assert_eq!(
        report.multi_unit_tenants, 1,
        "only Lovelace holds several units under one id"
    );
    assert!(report.flagged_groups.is_empty());
    assert_eq!(report.duplicate_customer_records.len(), 1);
    assert!(report.duplicate_customer_records[0].mismatches.is_empty());
    assert!(report.related_tenant_candidates.is_empty());
}
