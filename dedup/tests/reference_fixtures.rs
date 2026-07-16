//! Verifies `unitprep_dedup::report::run` against two real facility
//! exports, using expected values captured by actually re-running the
//! current reference Python script (`duplicate-tenant-check` skill,
//! installed-skill copy) against the same inputs on 2026-07-15 — see
//! project memory (`project-unitprep-dedup-tool`) for full provenance,
//! including the byte-for-byte reproduction of the independently
//! confirmed No Ka Oi result.
//!
//! These read real tenant PII directly from wherever it currently lives
//! on disk — never copied into the repo, and the repo itself holds no
//! opinion on where that is (facility data locations are exactly the
//! kind of thing that moves). Each test takes its file's path from an
//! environment variable instead of a hardcoded path, and both are
//! `#[ignore]`d by default:
//!
//!     UNITPREP_DEDUP_KAH_FIXTURE="/path/to/KAH_QMS_End_Users_Template.csv" \
//!     UNITPREP_DEDUP_NCSS_FIXTURE="/path/to/NCSS_QMS_End_Users_Template.csv" \
//!     cargo test -p unitprep-dedup -- --ignored

use std::fs;
use std::path::Path;

use unitprep_core::parsing::parse_document;
use unitprep_core::uploaded_file::UploadedFile;
use unitprep_dedup::ingest::records_from_csv_document;
use unitprep_dedup::report::run;
use unitprep_dedup::similarity::VARIANT_SURFACE_THRESHOLD;
use unitprep_dedup::types::{FieldCategory, TenantRecord};

fn load_records_from_env(env_var: &str) -> Vec<TenantRecord> {
    let path = std::env::var(env_var).unwrap_or_else(|_| {
        panic!("set {env_var} to the path of a real QMS End Users export to run this test")
    });
    let bytes = fs::read(&path)
        .unwrap_or_else(|e| panic!("{env_var} points at {path:?}, which failed to read: {e}"));
    let file_name = Path::new(&path)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let uploaded = UploadedFile { file_name, relative_path: String::new(), bytes };
    let document = parse_document(&uploaded).expect("fixture must parse as CSV");
    records_from_csv_document(&document).expect("fixture must have a FirtLast column")
}

fn unit_numbers(records: &[TenantRecord]) -> Vec<&str> {
    let mut units: Vec<&str> = records.iter().map(|r| r.unit_number.as_str()).collect();
    units.sort_unstable();
    units
}

#[test]
#[ignore]
fn no_ka_oi_matches_reference_script() {
    let records = load_records_from_env("UNITPREP_DEDUP_KAH_FIXTURE");
    let report = run(records);

    assert_eq!(report.total_rows, 292);
    assert_eq!(report.unique_tenants, 266);
    assert_eq!(report.multi_unit_tenants, 21);

    assert_eq!(report.flagged_groups.len(), 1);
    let flagged = &report.flagged_groups[0];
    assert_eq!(unit_numbers(&flagged.group.records), vec!["2008", "2123"]);
    assert_eq!(flagged.note, "Please update the email address to match across units 2008, 2123.");
    let categories: Vec<FieldCategory> = flagged.mismatches.iter().map(|m| m.category).collect();
    assert!(categories.contains(&FieldCategory::Email));
    assert!(categories.contains(&FieldCategory::AltContact));

    // Reference script auto-merged exactly 3 pairs and printed zero
    // review-only candidates for this file. Its merge rule is
    // `ratio >= 90% OR contact_info_matches` — not "ratio >= 90%" alone
    // — so two of these three (Paula Bacay ~88%, Barbara Smith ~87%)
    // were only merged because their contact info already matched, not
    // because of a high ratio. This crate doesn't distinguish a
    // confidence tier at all: all three clear VARIANT_SURFACE_THRESHOLD
    // and have matching contact info, which is the actual invariant the
    // reference script's output confirms.
    assert_eq!(report.typo_variant_candidates.len(), 3);
    for candidate in &report.typo_variant_candidates {
        assert!(candidate.ratio >= VARIANT_SURFACE_THRESHOLD);
        assert!(candidate.contact_info_matches);
        // Composed notes are now per-pair (real names/units filled in),
        // so assert the template that was selected rather than one
        // fixed string shared by all three.
        assert!(candidate.note.contains("may be the same tenant"));
        assert!(candidate.note.contains("all other contact info matches"));
    }
}

#[test]
#[ignore]
fn new_castle_matches_reference_script() {
    let records = load_records_from_env("UNITPREP_DEDUP_NCSS_FIXTURE");
    let report = run(records);

    assert_eq!(report.total_rows, 168);
    assert_eq!(report.unique_tenants, 116);
    assert_eq!(report.multi_unit_tenants, 15);

    assert_eq!(report.flagged_groups.len(), 1);
    let flagged = &report.flagged_groups[0];
    assert_eq!(unit_numbers(&flagged.group.records), vec!["F3", "F5"]);
    assert_eq!(
        flagged.note,
        "Please update the alternate contact info to match across units F3, F5."
    );
    let categories: Vec<FieldCategory> = flagged.mismatches.iter().map(|m| m.category).collect();
    assert_eq!(categories, vec![FieldCategory::AltContact]);

    assert_eq!(report.typo_variant_candidates.len(), 0);
}
