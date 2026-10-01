//! Shared vendor/PMS export-format recognition, used by every tool that
//! ingests a third-party export (Group Prep's unit files, dedup's
//! tenant files, ...). Vendor definitions are DATA — rows in
//! `client_ops.vendor_format`, loaded by the binary crate and passed in
//! as a plain `&[VendorFormat]` — not hardcoded per tool the way
//! Group Prep's own QSX/Storage Commander/DoorSwap consts used to be.
//! This module only holds the recognition/mapping mechanics; each
//! calling crate still owns its own canonical target-field list (what
//! its pipeline actually requires), since that's genuinely tool-specific.
//!
//! Per the original design this generalizes: every vendor, including
//! whichever one happens to be the common case for a given tool, goes
//! through the same recognize → map flow — no vendor is special-cased
//! as "just works," and no vendor-specific branching exists anywhere
//! outside this module and `transforms` below. Adding a vendor is a
//! pure data addition (one more `client_ops.vendor_format` row); adding
//! a *tool* is a pure data addition too (one more `ContentType`
//! variant).

use crate::csv_document::CsvDocument;

/// Which tool's pipeline a `VendorFormat` row feeds. Not exhaustively
/// matched anywhere outside loading/display code — a third value later
/// (a third tool, or a new export kind) is a data fact, not a reason to
/// touch recognition logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ContentType {
    Units,
    Tenants,
}

impl ContentType {
    pub fn as_db_str(self) -> &'static str {
        match self {
            ContentType::Units => "units",
            ContentType::Tenants => "tenants",
        }
    }

    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "units" => Some(ContentType::Units),
            "tenants" => Some(ContentType::Tenants),
            _ => None,
        }
    }
}

/// One recognized vendor/PMS export shape — the Rust-side mirror of a
/// `client_ops.vendor_format` row. Owned strings throughout (rather than
/// the `&'static str` an earlier, unit-group-only version of this type
/// used), since every row now comes from the same DB-loaded `Vec` —
/// there's no compile-time-const tier anymore to justify borrowing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VendorFormat {
    pub name: String,
    pub content_type: ContentType,
    /// Headers that must all be present (via `CsvDocument::header_index`,
    /// so case/separator-insensitive) for a document to be recognized as
    /// this vendor's export.
    pub signature_headers: Vec<String>,
    /// (canonical target field, this vendor's own header for it) pairs,
    /// hand-authored per vendor rather than derived by matching target
    /// names against the vendor's own headers — a vendor's raw
    /// vocabulary is often itself one of the canonical names under a
    /// different vendor's mapping, so name-matching would silently
    /// leave required fields unmapped.
    pub field_mapping: Vec<(String, String)>,
    /// Key into `transforms::apply`, run against the raw document
    /// before `field_mapping`'s rename step. `None` for the overwhelming
    /// majority of vendors — see `transforms`' doc comment for why this
    /// is the one place vendor-specific *code* (as opposed to data) is
    /// allowed to exist at all.
    pub transform_key: Option<String>,
}

/// Returns the first candidate whose full signature is present in
/// `document`'s headers, or `None` if it matches none of them. Caller
/// controls `candidates`' ordering, and therefore which vendor wins when
/// one signature is a strict superset of another's (e.g. Storage
/// Commander's real export also satisfies plain QSX's signature —
/// ordering Storage Commander first in the DB rows for that
/// `content_type` is what resolves that, not logic here).
pub fn detect_vendor<'a>(
    document: &CsvDocument,
    candidates: &'a [VendorFormat],
) -> Option<&'a VendorFormat> {
    candidates.iter().find(|vendor| {
        vendor
            .signature_headers
            .iter()
            .all(|header| document.header_index(header).is_some())
    })
}

/// Builds a new `CsvDocument` containing only the canonical target
/// fields `vendor.field_mapping` actually maps to a real source column —
/// each row's value pulled from that source column. A target with no
/// resolved source column is dropped entirely, never included as an
/// always-blank column: a caller whose optional-field checks treat
/// "this header exists" as "this vendor supplies real data for it" would
/// otherwise flag every row for a field this vendor simply never had —
/// the exact bug Group Prep's own DoorSwap onboarding hit before this
/// rule was written down.
///
/// When `vendor.transform_key` is set, that transform runs against
/// `document` first — its injected/rewritten columns are what
/// `field_mapping`'s ordinary identity-rename entries then pick up, so
/// this function itself never needs to know about any vendor's specific
/// column quirks.
pub fn apply_field_mapping(
    document: &CsvDocument,
    vendor: &VendorFormat,
) -> anyhow::Result<CsvDocument> {
    let transformed = match vendor.transform_key.as_deref() {
        Some(key) => transforms::apply(key, document)?,
        None => document.clone(),
    };

    let mapped: Vec<(&str, usize)> = vendor
        .field_mapping
        .iter()
        .filter_map(|(target, source)| {
            let index = transformed.header_index(source)?;
            Some((target.as_str(), index))
        })
        .collect();

    let source_indices: Vec<usize> = mapped.iter().map(|(_, index)| *index).collect();
    let headers: Vec<String> = mapped
        .iter()
        .map(|(target, _)| target.to_string())
        .collect();

    let rows: Vec<Vec<String>> = transformed
        .rows
        .iter()
        .map(|row| {
            source_indices
                .iter()
                .map(|&index| row.get(index).cloned().unwrap_or_default())
                .collect()
        })
        .collect();

    Ok(CsvDocument {
        file_name: transformed.file_name.clone(),
        headers,
        rows,
        modified_at: transformed.modified_at,
    })
}

pub mod transforms;

#[cfg(test)]
mod tests {
    use super::*;

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

    fn vendor(name: &str, signature: &[&str], mapping: &[(&str, &str)]) -> VendorFormat {
        VendorFormat {
            name: name.to_string(),
            content_type: ContentType::Units,
            signature_headers: signature.iter().map(|s| s.to_string()).collect(),
            field_mapping: mapping
                .iter()
                .map(|(t, s)| (t.to_string(), s.to_string()))
                .collect(),
            transform_key: None,
        }
    }

    #[test]
    fn detects_the_first_candidate_whose_full_signature_is_present() {
        let doc = document(
            vec!["Unit", "Unit Type", "Status", "Customer"],
            vec![vec!["1", "10x10", "Active", "Jane"]],
        );
        let candidates = vec![
            vendor("QSX", &["UnitGroup", "Number"], &[]),
            vendor(
                "DoorSwap",
                &["Unit", "Unit Type", "Status", "Customer"],
                &[],
            ),
        ];

        let detected = detect_vendor(&doc, &candidates).expect("DoorSwap should match");
        assert_eq!(detected.name, "DoorSwap");
    }

    #[test]
    fn detects_none_when_no_signature_fully_matches() {
        let doc = document(vec!["SomethingElse"], vec![vec!["x"]]);
        let candidates = vec![vendor("QSX", &["UnitGroup", "Number"], &[])];
        assert!(detect_vendor(&doc, &candidates).is_none());
    }

    #[test]
    fn ordering_lets_a_superset_signature_win_over_a_subset() {
        // Storage Commander's real export also satisfies QSX's own
        // (smaller) signature -- listing Storage Commander first is
        // what resolves that, exactly as it did when this lived in
        // unit-group's own hardcoded VENDOR_FORMATS array.
        let doc = document(
            vec!["UnitGroup", "Number", "Category", "Locality"],
            vec![vec!["10x10", "1", "Standard", "Inside"]],
        );
        let candidates = vec![
            vendor(
                "Storage Commander",
                &["UnitGroup", "Number", "Category", "Locality"],
                &[],
            ),
            vendor("QSX", &["UnitGroup", "Number", "Category"], &[]),
        ];

        let detected = detect_vendor(&doc, &candidates).expect("one of them must match");
        assert_eq!(detected.name, "Storage Commander");
    }

    #[test]
    fn apply_field_mapping_renames_and_drops_unmapped_targets() {
        let doc = document(
            vec!["Unit", "Unit Type"],
            vec![vec!["101", "10x10 Climate"]],
        );
        let v = vendor(
            "DoorSwap",
            &["Unit", "Unit Type"],
            &[
                ("Number", "Unit"),
                ("UnitGroup", "Unit Type"),
                ("Width", "Width"),
            ],
        );

        let mapped = apply_field_mapping(&doc, &v).expect("mapping should succeed");

        assert_eq!(mapped.headers, vec!["Number", "UnitGroup"]);
        assert_eq!(mapped.rows[0], vec!["101", "10x10 Climate"]);
    }

    #[test]
    fn apply_field_mapping_surfaces_an_unknown_transform_key_as_an_error() {
        let doc = document(vec!["Unit"], vec![vec!["101"]]);
        let mut v = vendor("Mystery", &["Unit"], &[("Number", "Unit")]);
        v.transform_key = Some("not_a_real_transform".to_string());

        let err = apply_field_mapping(&doc, &v).expect_err("unknown transform key must error");
        assert!(err.to_string().contains("not_a_real_transform"));
    }
}
