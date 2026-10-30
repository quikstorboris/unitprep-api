//! Runs two real QuikStor Cloud pulls (`Tenants.csv` + `Leases.csv`)
//! through dedup to check that the leases give every record a real unit.
//! Davidson Road writes one `Tenants.csv` row per tenant (344 leases for
//! 291 tenants); Freeland repeats the tenant once per lease (129 leases
//! for 107 tenants). Both must end as one record per lease.
//!
//! Two sets of tests. The ones on `tests/fixtures/quikstor_cloud/` run every
//! time: redacted copies of the same four files (names, phones, emails and
//! addresses replaced by stable placeholders that keep which rows share a
//! value; lease columns other than the ids, unit and start date blanked).
//! The others read the real files, which hold tenant PII, from wherever
//! they live, are `#[ignore]`d, and each take their folder from an
//! environment variable instead of a hardcoded path:
//!
//!     UNITPREP_DEDUP_QC_DAVIDSON_DIR="/path/to/1st Prelim Data" \
//!     UNITPREP_DEDUP_QC_FREELAND_DIR="/path/to/Freeland ... 2026-10-27" \
//!     cargo test -p unitprep-dedup --test quikstor_cloud_leases_fixtures -- --ignored

use std::fs;
use std::path::Path;

use unitprep_core::parsing::parse_document;
use unitprep_core::uploaded_file::UploadedFile;
use unitprep_core::vendor_format::{ContentType, VendorFormat};
use unitprep_dedup::ingest::records_from_documents;
use unitprep_dedup::report::run;

fn vendor(
    name: &str,
    signature: &[&str],
    mapping: &[(&str, &str)],
    transform: Option<&str>,
) -> VendorFormat {
    VendorFormat {
        name: name.to_string(),
        content_type: ContentType::Tenants,
        signature_headers: signature.iter().map(|h| h.to_string()).collect(),
        field_mapping: mapping
            .iter()
            .map(|(t, s)| (t.to_string(), s.to_string()))
            .collect(),
        transform_key: transform.map(String::from),
    }
}

/// The three registry rows these pulls use, mirroring the seed migrations
/// (`20261001120000`, `20261007150000`, `20261030120000`).
fn quikstor_cloud_vendors() -> Vec<VendorFormat> {
    let derive = Some("derive_quikstor_cloud_tenant_fields");
    let common = [
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
    ];
    let with = |extra: &[(&'static str, &'static str)]| -> Vec<(&'static str, &'static str)> {
        common.iter().chain(extra).copied().collect()
    };
    vec![
        vendor(
            "QuikStor Cloud",
            &[
                "LegacyTenantId",
                "AccountType",
                "FirstName",
                "LastName",
                "AddressLine",
                "CellPhoneNumber",
            ],
            &with(&[
                ("AddressStreet1", "AddressLine"),
                ("AddressStreet2", "AddressLineOptional"),
                ("AddressCity", "City"),
                ("AddressState", "State"),
                ("AddressPostalCode", "PostalCode"),
            ]),
            derive,
        ),
        vendor(
            "QuikStor Cloud (street address headers)",
            &[
                "LegacyTenantId",
                "AccountType",
                "FirstName",
                "LastName",
                "AddressStreet1",
                "CellPhoneNumber",
            ],
            &with(&[
                ("AddressStreet1", "AddressStreet1"),
                ("AddressStreet2", "AddressStreet2"),
                ("AddressCity", "AddressCity"),
                ("AddressState", "AddressState"),
                ("AddressPostalCode", "AddressPostalCode"),
            ]),
            derive,
        ),
        vendor(
            "QuikStor Cloud Leases",
            &[
                "LegacyLeaseId",
                "LegacyTenantId",
                "LegacyUnitId",
                "UnitNumber",
            ],
            &[("TenantId", "LegacyTenantId"), ("UnitNumber", "UnitNumber")],
            None,
        ),
    ]
}

fn read(dir: &Path, file: &str) -> unitprep_core::csv_document::CsvDocument {
    let path = dir.join(file);
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("{path:?} failed to read: {e}"));
    parse_document(&UploadedFile {
        file_name: file.to_string(),
        relative_path: String::new(),
        bytes,
        modified_at: None,
    })
    .unwrap_or_else(|e| panic!("{path:?} must parse as CSV: {e}"))
}

/// One record per lease, a real unit on every record, and the same set of
/// tenants as without the leases.
fn check(dir: &Path, leases: usize, tenants: usize, multi_unit: usize, duplicates: usize) {
    let tenants_doc = read(dir, "Tenants.csv");
    let leases_doc = read(dir, "Leases.csv");
    let vendors = quikstor_cloud_vendors();

    let joined = records_from_documents(&tenants_doc, &[&leases_doc], &vendors)
        .expect("tenants and leases must join");
    assert_eq!(joined.len(), leases, "one record per lease");
    assert!(
        joined.iter().all(|r| r.unit_number != r.tenant_id),
        "no record is left naming its tenant ID as its unit"
    );

    let report = run(joined);
    assert_eq!(report.unique_tenants, tenants);
    assert_eq!(
        report.multi_unit_tenants, multi_unit,
        "tenants holding 2+ units"
    );
    assert!(
        report.flagged_groups.is_empty(),
        "same details across a tenant's units: nothing to flag"
    );
    assert_eq!(report.duplicate_customer_records.len(), duplicates);
    println!(
        "{}: records {} tenants {} multi-unit {} flagged {} duplicate-customer {}",
        dir.display(),
        report.total_rows,
        report.unique_tenants,
        report.multi_unit_tenants,
        report.flagged_groups.len(),
        report.duplicate_customer_records.len()
    );
}

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/quikstor_cloud")
        .join(name)
}

fn real(dir_var: &str) -> std::path::PathBuf {
    std::env::var(dir_var)
        .unwrap_or_else(|_| panic!("set {dir_var} to a real QuikStor Cloud pull folder"))
        .into()
}

#[test]
fn redacted_davidson_road_leases_give_one_record_per_lease() {
    check(&fixture("davidson"), 344, 291, 42, 4);
}

#[test]
fn redacted_freeland_leases_give_one_record_per_lease() {
    check(&fixture("freeland"), 129, 107, 19, 7);
}

#[test]
#[ignore]
fn davidson_road_leases_give_one_record_per_lease() {
    check(&real("UNITPREP_DEDUP_QC_DAVIDSON_DIR"), 344, 291, 42, 4);
}

#[test]
#[ignore]
fn freeland_leases_give_one_record_per_lease() {
    check(&real("UNITPREP_DEDUP_QC_FREELAND_DIR"), 129, 107, 19, 7);
}
