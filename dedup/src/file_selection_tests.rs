use super::*;
use unitprep_core::vendor_format::ContentType;

fn vendor(name: &str, signature: &[&str]) -> VendorFormat {
    VendorFormat {
        name: name.to_string(),
        content_type: ContentType::Tenants,
        signature_headers: signature.iter().map(|s| s.to_string()).collect(),
        field_mapping: Vec::new(),
        transform_key: None,
    }
}

fn meta(name: &str, pms: &str, role: FileRole, priority: i32) -> FileFormatMeta {
    FileFormatMeta {
        name: name.to_string(),
        pms: pms.to_string(),
        report_name: name.to_string(),
        role,
        selection_priority: priority,
        guidance: String::new(),
    }
}

fn file(name: &str, headers: Option<&[&str]>) -> FileHeaders {
    FileHeaders {
        file_name: name.to_string(),
        path: None,
        headers: headers.map(|h| h.iter().map(|s| s.to_string()).collect()),
    }
}

const SITELINK_COMMON: [&str; 7] = [
    "sUnitName",
    "LedgerID",
    "TenantID",
    "sFName",
    "sLName",
    "sAddr1",
    "sEmail",
];

/// Registry in detection order, shaped like the seeded rows: the more
/// specific signature sits before the one it would otherwise shadow.
fn registry() -> (Vec<VendorFormat>, Vec<FileFormatMeta>) {
    let mut directory = SITELINK_COMMON.to_vec();
    directory.push("TenantName");
    let qc = [
        "LegacyTenantId",
        "AccountType",
        "FirstName",
        "LastName",
        "AddressLine",
        "CellPhoneNumber",
    ];
    let mut qc_alt = qc.to_vec();
    qc_alt.push("LegacyAlternateTenantId");

    let vendors = vec![
        vendor("QSX", &["FirtLast", "CustNumb", "AddressStreet1"]),
        vendor("QuikStor Cloud Alternate Tenants", &qc_alt),
        vendor("QuikStor Cloud", &qc),
        vendor("SiteLink Directory", &directory),
        vendor("SiteLink Rent Roll", &SITELINK_COMMON),
    ];
    let metas = vec![
        meta("QSX", "QSX", FileRole::Primary, 0),
        meta(
            "QuikStor Cloud Alternate Tenants",
            "QuikStor Cloud",
            FileRole::Supporting,
            0,
        ),
        meta("QuikStor Cloud", "QuikStor Cloud", FileRole::Primary, 0),
        meta("SiteLink Directory", "SiteLink", FileRole::Primary, 20),
        meta("SiteLink Rent Roll", "SiteLink", FileRole::Primary, 10),
    ];
    (vendors, metas)
}

fn sitelink_headers(with_tenant_name: bool) -> Vec<&'static str> {
    let mut h = SITELINK_COMMON.to_vec();
    h.extend(["sPhone", "sMobile"]);
    if with_tenant_name {
        h.push("TenantName");
    }
    h
}

#[test]
fn directory_is_preselected_over_rent_roll_and_rent_roll_is_an_alternative() {
    let (vendors, metas) = registry();
    let files = vec![
        file("Rent Roll.xlsx", Some(&sitelink_headers(false))),
        file("Directory.xlsx", Some(&sitelink_headers(true))),
        file("Gate Access.xlsx", Some(&["Unit", "GateCode"])),
    ];

    let (classified, suggestion) = classify(&files, &vendors, &metas);

    assert_eq!(
        classified[0].format.as_ref().unwrap().name,
        "SiteLink Rent Roll"
    );
    assert_eq!(
        classified[1].format.as_ref().unwrap().name,
        "SiteLink Directory"
    );
    assert_eq!(classified[2].status, FileStatus::Unrecognized);
    assert_eq!(suggestion.pms.as_deref(), Some("SiteLink"));
    assert_eq!(suggestion.selected, vec!["Directory.xlsx"]);
    assert_eq!(
        suggestion.alternatives,
        vec![("Rent Roll.xlsx".to_string(), "Directory.xlsx".to_string())]
    );
}

#[test]
fn header_matching_ignores_case_and_separators_like_a_real_ingest() {
    let (vendors, metas) = registry();
    let files = vec![file(
        "export.csv",
        Some(&["firt_last", "CUSTNUMB", "Address Street1"]),
    )];

    let (classified, _) = classify(&files, &vendors, &metas);

    assert_eq!(classified[0].status, FileStatus::Recognized);
    assert_eq!(classified[0].format.as_ref().unwrap().name, "QSX");
}

#[test]
fn an_alternate_contacts_file_is_supporting_and_never_preselected() {
    let (vendors, metas) = registry();
    let tenants = [
        "LegacyTenantId",
        "AccountType",
        "FirstName",
        "LastName",
        "AddressLine",
        "CellPhoneNumber",
    ];
    let mut alternate = tenants.to_vec();
    alternate.extend(["Relationship", "LegacyAlternateTenantId"]);
    let files = vec![
        file("AlternateTenants.csv", Some(&alternate)),
        file("Tenants.csv", Some(&tenants)),
    ];

    let (classified, suggestion) = classify(&files, &vendors, &metas);

    assert_eq!(
        classified[0].format.as_ref().unwrap().role,
        FileRole::Supporting
    );
    assert_eq!(
        classified[1].format.as_ref().unwrap().name,
        "QuikStor Cloud"
    );
    assert_eq!(suggestion.selected, vec!["Tenants.csv"]);
    assert!(suggestion.alternatives.is_empty());
}

#[test]
fn only_supporting_files_still_name_the_pms_but_select_nothing() {
    let (vendors, metas) = registry();
    let mut alternate = vec![
        "LegacyTenantId",
        "AccountType",
        "FirstName",
        "LastName",
        "AddressLine",
        "CellPhoneNumber",
    ];
    alternate.push("LegacyAlternateTenantId");
    let files = vec![file("AlternateTenants.csv", Some(&alternate))];

    let (_, suggestion) = classify(&files, &vendors, &metas);

    assert_eq!(suggestion.pms.as_deref(), Some("QuikStor Cloud"));
    assert!(suggestion.selected.is_empty());
}

#[test]
fn unreadable_files_are_reported_and_never_selected() {
    let (vendors, metas) = registry();
    let files = vec![file("legacy.xls", None)];

    let (classified, suggestion) = classify(&files, &vendors, &metas);

    assert_eq!(classified[0].status, FileStatus::Unreadable);
    assert!(suggestion.selected.is_empty());
    assert_eq!(suggestion.pms, None);
}

#[test]
fn the_pms_with_the_highest_priority_primary_wins_when_a_folder_mixes_systems() {
    let (vendors, metas) = registry();
    let files = vec![
        file("a.csv", Some(&["FirtLast", "CustNumb", "AddressStreet1"])),
        file("Directory.xlsx", Some(&sitelink_headers(true))),
    ];

    let (_, suggestion) = classify(&files, &vendors, &metas);

    assert_eq!(suggestion.pms.as_deref(), Some("SiteLink"));
    assert_eq!(suggestion.selected, vec!["Directory.xlsx"]);
}

fn detected<'a>(name: &'a str, format: Option<FileFormatMeta>) -> DetectedFile<'a> {
    DetectedFile {
        file_name: name,
        format,
    }
}

#[test]
fn a_single_recognized_primary_file_can_run() {
    let files = [detected(
        "d.xlsx",
        Some(meta(
            "SiteLink Directory",
            "SiteLink",
            FileRole::Primary,
            20,
        )),
    )];
    assert_eq!(
        plan_ingest(&files),
        Ok(IngestPlan {
            primary: 0,
            joins: vec![]
        })
    );
}

#[test]
fn selecting_nothing_is_an_error() {
    assert_eq!(plan_ingest(&[]), Err(SelectionError::NoFiles));
}

#[test]
fn an_unrecognized_file_blocks_the_run() {
    let files = [detected("mystery.csv", None)];
    assert_eq!(
        plan_ingest(&files),
        Err(SelectionError::Unrecognized {
            file: "mystery.csv".into()
        })
    );
}

#[test]
fn a_supporting_file_cannot_run_alone() {
    let files = [detected(
        "AlternateTenants.csv",
        Some(meta(
            "QuikStor Cloud Alternate Tenants",
            "QuikStor Cloud",
            FileRole::Supporting,
            0,
        )),
    )];
    assert!(matches!(
        plan_ingest(&files),
        Err(SelectionError::Supporting { .. })
    ));
}

#[test]
fn two_alternatives_of_one_system_are_refused_to_avoid_double_counting() {
    let files = [
        detected(
            "d.xlsx",
            Some(meta(
                "SiteLink Directory",
                "SiteLink",
                FileRole::Primary,
                20,
            )),
        ),
        detected(
            "r.xlsx",
            Some(meta(
                "SiteLink Rent Roll",
                "SiteLink",
                FileRole::Primary,
                10,
            )),
        ),
    ];
    let err = plan_ingest(&files).unwrap_err();
    assert!(matches!(err, SelectionError::Alternatives { .. }));
    assert!(err.to_string().contains("count them twice"));
}

#[test]
fn the_same_kind_of_file_twice_is_refused() {
    let files = [
        detected("a.csv", Some(meta("QSX", "QSX", FileRole::Primary, 0))),
        detected("b.csv", Some(meta("QSX", "QSX", FileRole::Primary, 0))),
    ];
    assert!(matches!(
        plan_ingest(&files),
        Err(SelectionError::DuplicateFormat { .. })
    ));
}

#[test]
fn files_from_two_systems_are_refused() {
    let files = [
        detected("a.csv", Some(meta("QSX", "QSX", FileRole::Primary, 0))),
        detected(
            "d.xlsx",
            Some(meta(
                "SiteLink Directory",
                "SiteLink",
                FileRole::Primary,
                20,
            )),
        ),
    ];
    assert!(matches!(
        plan_ingest(&files),
        Err(SelectionError::MixedSystems { .. })
    ));
}

fn winsen(role: FileRole, name: &str) -> FileFormatMeta {
    meta(name, "Winsen", role, 0)
}

#[test]
fn join_files_ride_along_with_their_primary() {
    let files = [
        detected(
            "rentroll.xls",
            Some(winsen(FileRole::Join, "Winsen Rent Roll")),
        ),
        detected(
            "xref.xls",
            Some(winsen(FileRole::Primary, "Winsen Cross Reference")),
        ),
        detected("email.xls", Some(winsen(FileRole::Join, "Winsen Email"))),
    ];
    assert_eq!(
        plan_ingest(&files),
        Ok(IngestPlan {
            primary: 1,
            joins: vec![0, 2]
        })
    );
}

#[test]
fn a_join_file_alone_is_refused_with_its_name() {
    let files = [detected(
        "email.xls",
        Some(winsen(FileRole::Join, "Winsen Email")),
    )];
    assert!(matches!(
        plan_ingest(&files),
        Err(SelectionError::JoinWithoutPrimary { .. })
    ));
}

#[test]
fn a_join_file_from_another_system_is_refused() {
    let files = [
        detected(
            "xref.xls",
            Some(winsen(FileRole::Primary, "Winsen Cross Reference")),
        ),
        detected(
            "e.csv",
            Some(meta("Other Email", "Other", FileRole::Join, 0)),
        ),
    ];
    assert!(matches!(
        plan_ingest(&files),
        Err(SelectionError::MixedSystems { .. })
    ));
}

#[test]
fn two_join_files_of_one_kind_are_refused() {
    let files = [
        detected(
            "xref.xls",
            Some(winsen(FileRole::Primary, "Winsen Cross Reference")),
        ),
        detected("email1.xls", Some(winsen(FileRole::Join, "Winsen Email"))),
        detected("email2.xls", Some(winsen(FileRole::Join, "Winsen Email"))),
    ];
    assert!(matches!(
        plan_ingest(&files),
        Err(SelectionError::DuplicateFormat { .. })
    ));
}

#[test]
fn the_pre_selection_ticks_the_join_files_of_the_chosen_system() {
    let vendors = vec![
        vendor(
            "Winsen Cross Reference",
            &["Unit", "Customer Name", "Address Line 1"],
        ),
        vendor(
            "Winsen Email",
            &["Unit", "Customer Name", "Customer Email Address"],
        ),
        vendor("Winsen Rent Roll", &["Unit", "Customer Name", "Cust ID"]),
    ];
    let metas = vec![
        winsen(FileRole::Primary, "Winsen Cross Reference"),
        winsen(FileRole::Join, "Winsen Email"),
        winsen(FileRole::Join, "Winsen Rent Roll"),
    ];
    let files = [
        file(
            "xref.xls",
            Some(&["Unit", "Customer Name", "Address Line 1"]),
        ),
        file(
            "email.xls",
            Some(&["Unit", "Customer Name", "Customer Email Address"]),
        ),
        file("rentroll.xls", Some(&["Unit", "Customer Name", "Cust ID"])),
        file("notes.xls", Some(&["Note"])),
    ];

    let (_, suggestion) = classify(&files, &vendors, &metas);

    assert_eq!(suggestion.pms.as_deref(), Some("Winsen"));
    assert_eq!(
        suggestion.selected,
        ["xref.xls", "email.xls", "rentroll.xls"]
    );
}
