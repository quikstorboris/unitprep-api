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
        .filter_map(|(header, setter)| normalized.header_index(header).map(|idx| (idx, *setter)))
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
#[path = "ingest_tests.rs"]
mod tests;
