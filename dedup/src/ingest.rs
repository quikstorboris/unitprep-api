//! Builds `TenantRecord`s from a parsed `CsvDocument`. Reuses
//! `unitprep-core`'s parsing and `header_index` — this crate never
//! re-reads files from disk or grows its own header-name matching, per
//! UnitPrep's parse-once policy and its header-normalization bug
//! history (see project memory).
//!
//! Used to hard-require QSX's own `FirtLast` column and error otherwise
//! — the only tenant-export shape this crate had ever seen. Onboarding
//! Easy Storage Solutions (a completely different column vocabulary for
//! the same underlying tenant/unit/contact concepts) means every vendor
//! now goes through the same detect-then-normalize step, QSX included —
//! the same generalization Group Prep's own unit-file discovery went
//! through first (see `unitprep_core::vendor_format`). This module still
//! has zero vendor-specific branching: `COLUMNS` below reads canonical
//! header names only, and `detect_vendor`/`apply_field_mapping` are what
//! turn a vendor's own raw headers into those canonical names before
//! this ever sees the document.

use anyhow::{Context, Result};
use unitprep_core::csv_document::CsvDocument;
use unitprep_core::vendor_format::{apply_field_mapping, detect_vendor, VendorFormat};

use crate::types::TenantRecord;

/// A column's setter: writes one parsed field value into a `TenantRecord`.
type ColumnSetter = fn(&mut TenantRecord, String);

/// QMS export columns this crate reads, and the `TenantRecord` field
/// each populates. Looked up via `CsvDocument::header_index`, so exact
/// header spelling/casing/separators in the source file don't matter.
/// These are canonical names — the same ones every registered vendor's
/// `field_mapping` maps its own raw headers onto — not any one vendor's
/// literal export vocabulary.
const COLUMNS: &[(&str, ColumnSetter)] = &[
    ("CustNumb", |r, v| r.cust_numb = v),
    ("UnitNumber", |r, v| r.unit_number = v),
    ("FirtLast", |r, v| r.first_last = v),
    ("FirstName", |r, v| r.first_name = v),
    ("LastName", |r, v| r.last_name = v),
    ("CompanyName", |r, v| r.company_name = v),
    ("PhoneNumber", |r, v| r.phone_number = v),
    ("PhoneNumberPrefix", |r, v| r.phone_number_prefix = v),
    ("Email", |r, v| r.email = v),
    ("AddressStreet1", |r, v| r.address_street1 = v),
    ("AddressStreet2", |r, v| r.address_street2 = v),
    ("AddressCity", |r, v| r.address_city = v),
    ("AddressState", |r, v| r.address_state = v),
    ("AddressPostalCode", |r, v| r.address_postal_code = v),
    ("AlternateContactFirstName", |r, v| {
        r.alt_contact_first_name = v
    }),
    ("AlternateContactLastName", |r, v| {
        r.alt_contact_last_name = v
    }),
    ("AlternateContactEmail", |r, v| r.alt_contact_email = v),
    ("AlternateContactPhoneNumber", |r, v| {
        r.alt_contact_phone_number = v
    }),
    ("AlternateContactPhoneNumberPrefix", |r, v| {
        r.alt_contact_phone_number_prefix = v
    }),
    ("AlternateContactAddressStreet1", |r, v| {
        r.alt_contact_address_street1 = v
    }),
    ("AlternateContactAddressStreet2", |r, v| {
        r.alt_contact_address_street2 = v
    }),
    ("AlternateContactAddressCity", |r, v| {
        r.alt_contact_address_city = v
    }),
    ("AlternateContactAddressState", |r, v| {
        r.alt_contact_address_state = v
    }),
    ("AlternateContactAddressPostalCode", |r, v| {
        r.alt_contact_address_postal_code = v
    }),
];

/// Detects which registered vendor `doc` came from (against
/// `tenant_vendors` — `client_ops.vendor_format` rows for
/// `content_type = 'tenants'`, loaded and cached by the caller), applies
/// that vendor's field mapping (running its transform first, if it has
/// one — see `AddressStreet1` et al. for Easy Storage Solutions), then
/// builds one `TenantRecord` per row from the now-canonically-named
/// document. Errors if no registered vendor's signature matches, or if
/// the matched vendor's mapping somehow doesn't produce a `FirtLast`
/// column (the grouping key, with no fallback) — every column past that
/// is optional and defaults to blank when missing, same tolerance the
/// reference script has via `dict.get(field, "")`.
pub fn records_from_csv_document(
    doc: &CsvDocument,
    tenant_vendors: &[VendorFormat],
) -> Result<Vec<TenantRecord>> {
    let vendor = detect_vendor(doc, tenant_vendors).with_context(|| {
        let known: Vec<&str> = tenant_vendors.iter().map(|v| v.name.as_str()).collect();
        format!(
            "Unrecognized tenant export format — this file's columns don't match a known vendor ({})",
            known.join(", ")
        )
    })?;

    let normalized = apply_field_mapping(doc, vendor)
        .with_context(|| format!("Failed to normalize a '{}' export", vendor.name))?;

    normalized
        .header_index("FirtLast")
        .context("QMS export is missing the required FirtLast column")?;

    let resolved: Vec<(usize, ColumnSetter)> = COLUMNS
        .iter()
        .filter_map(|(header, setter)| {
            normalized
                .header_index(header)
                .map(|idx| (idx, *setter))
        })
        .collect();

    Ok(normalized
        .rows
        .iter()
        .map(|row| {
            let mut record = TenantRecord::default();
            for (idx, setter) in &resolved {
                if let Some(value) = row.get(*idx) {
                    setter(&mut record, value.clone());
                }
            }
            record
        })
        .collect())
}

#[cfg(test)]
mod tests {
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
            field_mapping: columns.iter().map(|c| (c.to_string(), c.to_string())).collect(),
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
            vec!["CustNumb", "UnitNumber", "FirtLast", "Email", "AddressStreet1"],
            vec![vec!["C1", "101", "Doe, Jane", "jane@example.com", "1 Main St"]],
        );

        let records =
            records_from_csv_document(&doc, &qsx_vendors()).expect("known-good QSX document");

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
        let records =
            records_from_csv_document(&doc, &vendors).expect("known-good ESS document");

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

    #[test]
    fn refuses_a_document_that_matches_no_known_vendor() {
        let doc = document(vec!["CustNumb", "UnitNumber"], vec![vec!["C1", "101"]]);

        let err = records_from_csv_document(&doc, &qsx_vendors())
            .expect_err("headers don't satisfy any registered vendor's signature");

        assert!(err.to_string().contains("Unrecognized tenant export format"));
    }
}
