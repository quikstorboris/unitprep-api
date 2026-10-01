//! Checks the SEEDED registry (real rows, loaded through the real
//! loaders) against the header rows of real exports: QSX, QuikStor Cloud
//! (Tenants / AlternateTenants / Units) and SiteLink (Directory / Rent
//! Roll / unrelated reports). Headers only -- no tenant data -- so the
//! fixtures are safe to keep in the repo. This is what catches a
//! migration that seeds a signature in the wrong detection order, which
//! hand-built registries in unit tests cannot.

use uuid::Uuid;

use unitprep_core::vendor_format::ContentType;
use unitprep_dedup::file_selection::{classify, FileHeaders, FileRole, FileStatus};

use crate::client_ops::vendor_file_meta::load_file_meta;
use crate::client_ops::vendor_format::load_vendor_formats;

fn file(name: &str, headers: &[&str]) -> FileHeaders {
    FileHeaders {
        file_name: name.to_string(),
        path: None,
        headers: Some(headers.iter().map(|h| h.to_string()).collect()),
    }
}

const QC_TENANTS: [&str; 32] = [
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
    "OtherEmail",
    "AddressLine",
    "AddressLineOptional",
    "City",
    "State",
    "PostalCode",
    "Country",
    "SpecialTenantNote",
    "DriversLicense",
    "DriversLicenseState",
    "DateOfBirth",
    "Source",
    "Language",
    "MilesFromSite",
    "NSFCounter",
    "ETSDate",
    "UnitName",
    "UnitPhoneNumber",
    "CommandingOfficer",
];

fn with(base: &[&str], extra: &[&str]) -> Vec<String> {
    base.iter().chain(extra).map(|s| s.to_string()).collect()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real, reachable Postgres with migrations applied -- see dedup_session_service.rs's durability test for the rationale"]
async fn the_seeded_registry_classifies_real_export_headers() {
    let db = crate::db::connect_test();
    let vendors = load_vendor_formats(&db, Uuid::nil(), &[], ContentType::Tenants)
        .await
        .expect("vendors load");
    let metas = load_file_meta(&db, Uuid::nil(), &[], ContentType::Tenants)
        .await
        .expect("file metadata loads");

    let qc_alt = with(
        &QC_TENANTS,
        &[
            "Relationship",
            "OtherAllowedAccess",
            "LegacyAlternateTenantId",
        ],
    );
    let qc_alt: Vec<&str> = qc_alt.iter().map(String::as_str).collect();
    let sitelink = [
        "sUnitName",
        "LedgerID",
        "TenantID",
        "sFName",
        "sLName",
        "sCompany",
        "sAddr1",
        "sCity",
        "sRegion",
        "sPostalCode",
        "sPhone",
        "sMobile",
        "sEmail",
        "dcRent",
        "dPaidThru",
    ];
    let directory = with(&sitelink, &["TenantName", "TaxExempt", "EmployeeID"]);
    let directory: Vec<&str> = directory.iter().map(String::as_str).collect();
    let rent_roll = with(&sitelink, &["MarketID_Whse", "sCreditCardProvider"]);
    let rent_roll: Vec<&str> = rent_roll.iter().map(String::as_str).collect();

    let files = vec![
        file("Tenants.csv", &QC_TENANTS),
        file("AlternateTenants.csv", &qc_alt),
        file(
            "Units.csv",
            &[
                "Number",
                "LegacyUnitId",
                "UnitType",
                "StandardRate",
                "Status",
            ],
        ),
        file("Directory.xlsx", &directory),
        file("Rent Roll.xlsx", &rent_roll),
        file(
            "Credit Card Roll.xlsx",
            &["Unit", "Name", "CardType", "CardNumber", "sToken"],
        ),
        file(
            "QSX End Users.csv",
            &[
                "CustNumb",
                "UnitNumber",
                "FirtLast",
                "AddressStreet1",
                "Email",
            ],
        ),
    ];

    let (classified, _) = classify(&files, &vendors, &metas);
    let format_of = |name: &str| {
        classified
            .iter()
            .find(|c| c.file_name == name)
            .and_then(|c| c.format.as_ref())
            .map(|m| (m.name.clone(), m.role))
    };

    assert_eq!(
        format_of("Tenants.csv"),
        Some(("QuikStor Cloud".to_string(), FileRole::Primary))
    );
    assert_eq!(
        format_of("AlternateTenants.csv"),
        Some((
            "QuikStor Cloud Alternate Tenants".to_string(),
            FileRole::Supporting
        )),
        "the alternate-contacts file must not be mistaken for the tenant file"
    );
    assert_eq!(format_of("Units.csv"), None);
    assert_eq!(
        format_of("Directory.xlsx"),
        Some(("SiteLink Directory".to_string(), FileRole::Primary))
    );
    assert_eq!(
        format_of("Rent Roll.xlsx"),
        Some(("SiteLink Rent Roll".to_string(), FileRole::Primary))
    );
    assert_eq!(
        classified[5].status,
        FileStatus::Unrecognized,
        "a report holding card data is never a dedup input"
    );
    assert_eq!(
        format_of("QSX End Users.csv").map(|f| f.0),
        Some("QSX".to_string())
    );

    // Within a SiteLink-only folder the Directory is pre-selected over
    // the Rent Roll, and the Rent Roll is offered as the alternative.
    let sitelink_only: Vec<FileHeaders> = files
        .iter()
        .filter(|f| f.file_name == "Directory.xlsx" || f.file_name == "Rent Roll.xlsx")
        .cloned()
        .collect();
    let (_, suggestion) = classify(&sitelink_only, &vendors, &metas);
    assert_eq!(suggestion.selected, vec!["Directory.xlsx"]);
    assert_eq!(
        suggestion.alternatives,
        vec![("Rent Roll.xlsx".to_string(), "Directory.xlsx".to_string())]
    );

    // Formats whose export has a real tenant id map it into dedup's
    // canonical TenantId; formats that only have a per-unit customer
    // number (QSX, Easy Storage Solutions) must not.
    let source_of_tenant_id = |format: &str| {
        vendors
            .iter()
            .find(|v| v.name == format)
            .and_then(|v| {
                v.field_mapping
                    .iter()
                    .find(|(target, _)| target == "TenantId")
            })
            .map(|(_, source)| source.clone())
    };
    assert_eq!(
        source_of_tenant_id("SiteLink Directory"),
        Some("TenantID".to_string())
    );
    assert_eq!(
        source_of_tenant_id("SiteLink Rent Roll"),
        Some("TenantID".to_string())
    );
    assert_eq!(
        source_of_tenant_id("QuikStor Cloud"),
        Some("LegacyTenantId".to_string())
    );
    assert_eq!(source_of_tenant_id("QSX"), None);
    assert_eq!(source_of_tenant_id("Easy Storage Solutions"), None);

    // Every shipped format carries guidance for the requirements panel.
    for meta in &metas {
        assert!(!meta.guidance.is_empty(), "{} has no guidance", meta.name);
        assert!(!meta.pms.is_empty());
    }
}
