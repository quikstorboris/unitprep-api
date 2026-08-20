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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone)]
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

/// Real parsing logic for the rare vendor whose export packs several
/// canonical fields into one raw column in a shape no rename table can
/// express. Deliberately the ONLY place vendor-specific code is allowed
/// to exist — `client_ops.vendor_format` has no code column, so a
/// self-service "add a vendor" flow can never reach this module; a
/// custom vendor that turns out to need a real transform is a signal it
/// should graduate into a hand-authored row here, reviewed and tested
/// like any other code change, the same path Storage Commander and
/// DoorSwap followed before this table existed at all.
pub mod transforms {
    use crate::csv_document::CsvDocument;

    pub fn apply(key: &str, document: &CsvDocument) -> anyhow::Result<CsvDocument> {
        match key {
            "split_ess_address" => Ok(split_ess_address(document)),
            other => anyhow::bail!("unknown vendor-format transform key: {other}"),
        }
    }

    /// Easy Storage Solutions' `Address` column combines street and
    /// city/state/zip across an embedded newline inside one CSV field
    /// (`"208 Laurel Oak Dr.\nSt. Rose, Louisiana 70087"`). Splits it
    /// into the four canonical address fields the comparison pipeline
    /// expects as independent columns, appended onto the document under
    /// their canonical names — `apply_field_mapping`'s ordinary identity
    /// entries for those four fields then pick them straight up.
    ///
    /// A no-op (returns `document` unchanged) if there's no `Address`
    /// column at all, so this stays safe to call speculatively.
    fn split_ess_address(document: &CsvDocument) -> CsvDocument {
        let Some(address_idx) = document.header_index("Address") else {
            return document.clone();
        };

        let mut headers = document.headers.clone();
        headers.push("AddressStreet1".to_string());
        headers.push("AddressCity".to_string());
        headers.push("AddressState".to_string());
        headers.push("AddressPostalCode".to_string());

        let rows = document
            .rows
            .iter()
            .map(|row| {
                let mut row = row.clone();
                let raw = row.get(address_idx).cloned().unwrap_or_default();
                let (street, city, state, postal) = parse_multiline_address(&raw);
                row.push(street);
                row.push(city);
                row.push(state);
                row.push(postal);
                row
            })
            .collect();

        CsvDocument {
            file_name: document.file_name.clone(),
            headers,
            rows,
            modified_at: document.modified_at,
        }
    }

    /// Splits one raw `"street\nCity, State Zip"` value into its four
    /// parts. Verified against every row of a real Easy Storage
    /// Solutions export (160 rows: 154 two-line addresses, 6 blank) —
    /// see `vendor_format_tests.rs` for the fixture rows this was
    /// checked against, including a zip+4 (`"70301-6843"`) and a row
    /// with a city/state but no zip at all (`"Abita Springs, LA"`).
    ///
    /// Falls back gracefully rather than erroring wherever a row
    /// doesn't match the expected shape — a blank address, a missing
    /// second line, a missing comma, or a state/zip segment with no
    /// space all degrade to "put what we have in `street`, leave the
    /// rest blank" instead of failing the whole file over one malformed
    /// row. Splitting on the LAST space in the state/zip segment (not
    /// the first) is what makes multi-word states like "New York" or
    /// "North Carolina" split correctly — the zip is always the final
    /// token, however many words the state name has.
    fn parse_multiline_address(raw: &str) -> (String, String, String, String) {
        let mut lines = raw.splitn(2, '\n');
        let street = lines.next().unwrap_or("").trim().to_string();

        let Some(second_line) = lines.next() else {
            return (street, String::new(), String::new(), String::new());
        };
        let second_line = second_line.trim();

        let Some((city_part, state_zip)) = second_line.rsplit_once(',') else {
            return (street, String::new(), second_line.to_string(), String::new());
        };
        let city = city_part.trim().to_string();
        let state_zip = state_zip.trim();

        let Some((state, postal)) = state_zip.rsplit_once(' ') else {
            return (street, city, state_zip.to_string(), String::new());
        };

        (street, city, state.trim().to_string(), postal.trim().to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn splits_a_real_two_line_address() {
            let (street, city, state, postal) =
                parse_multiline_address("208 Laurel Oak Dr.\nSt. Rose, Louisiana 70087");
            assert_eq!(street, "208 Laurel Oak Dr.");
            assert_eq!(city, "St. Rose");
            assert_eq!(state, "Louisiana");
            assert_eq!(postal, "70087");
        }

        #[test]
        fn handles_a_multi_word_state_name() {
            let (_, city, state, postal) =
                parse_multiline_address("1 Main St\nThibodaux, North Carolina 70301");
            assert_eq!(city, "Thibodaux");
            assert_eq!(state, "North Carolina");
            assert_eq!(postal, "70301");
        }

        #[test]
        fn preserves_a_zip_plus_four() {
            let (_, _, _, postal) =
                parse_multiline_address("1218 EMPIRE BUILDER\nTHIBODAUX, LA 70301-6843");
            assert_eq!(postal, "70301-6843");
        }

        #[test]
        fn falls_back_when_the_second_line_has_no_zip() {
            let (street, city, state, postal) =
                parse_multiline_address("21211 Soell Dr.\nAbita Springs, LA");
            assert_eq!(street, "21211 Soell Dr.");
            assert_eq!(city, "Abita Springs");
            assert_eq!(state, "LA");
            assert_eq!(postal, "");
        }

        #[test]
        fn falls_back_on_a_blank_address() {
            let (street, city, state, postal) = parse_multiline_address("");
            assert_eq!(street, "");
            assert_eq!(city, "");
            assert_eq!(state, "");
            assert_eq!(postal, "");
        }

        #[test]
        fn split_ess_address_is_a_noop_without_an_address_column() {
            let doc = CsvDocument {
                file_name: "test.csv".to_string(),
                headers: vec!["Unit".to_string()],
                rows: vec![vec!["101".to_string()]],
                modified_at: None,
            };
            let result = split_ess_address(&doc);
            assert_eq!(result.headers, doc.headers);
        }

        #[test]
        fn split_ess_address_appends_the_four_canonical_columns() {
            let doc = CsvDocument {
                file_name: "test.csv".to_string(),
                headers: vec!["Unit".to_string(), "Address".to_string()],
                rows: vec![vec![
                    "101".to_string(),
                    "208 Laurel Oak Dr.\nSt. Rose, Louisiana 70087".to_string(),
                ]],
                modified_at: None,
            };
            let result = split_ess_address(&doc);
            assert_eq!(
                result.headers,
                vec!["Unit", "Address", "AddressStreet1", "AddressCity", "AddressState", "AddressPostalCode"]
            );
            assert_eq!(
                result.rows[0],
                vec!["101", "208 Laurel Oak Dr.\nSt. Rose, Louisiana 70087", "208 Laurel Oak Dr.", "St. Rose", "Louisiana", "70087"]
            );
        }
    }
}

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
            vendor("DoorSwap", &["Unit", "Unit Type", "Status", "Customer"], &[]),
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
            vendor("Storage Commander", &["UnitGroup", "Number", "Category", "Locality"], &[]),
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
            &[("Number", "Unit"), ("UnitGroup", "Unit Type"), ("Width", "Width")],
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
