//! Builds `TenantRecord`s from a parsed `CsvDocument`. Reuses
//! `unitprep-core`'s parsing and `header_index` — this crate never
//! re-reads files from disk or grows its own header-name matching, per
//! UnitPrep's parse-once policy and its header-normalization bug
//! history (see project memory).

use anyhow::{Context, Result};
use unitprep_core::csv_document::CsvDocument;

use crate::types::TenantRecord;

/// A column's setter: writes one parsed field value into a `TenantRecord`.
type ColumnSetter = fn(&mut TenantRecord, String);

/// QMS export columns this crate reads, and the `TenantRecord` field
/// each populates. Looked up via `CsvDocument::header_index`, so exact
/// header spelling/casing/separators in the source file don't matter.
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
    ("AlternateContactFirstName", |r, v| r.alt_contact_first_name = v),
    ("AlternateContactLastName", |r, v| r.alt_contact_last_name = v),
    ("AlternateContactEmail", |r, v| r.alt_contact_email = v),
    ("AlternateContactPhoneNumber", |r, v| r.alt_contact_phone_number = v),
    ("AlternateContactPhoneNumberPrefix", |r, v| r.alt_contact_phone_number_prefix = v),
    ("AlternateContactAddressStreet1", |r, v| r.alt_contact_address_street1 = v),
    ("AlternateContactAddressStreet2", |r, v| r.alt_contact_address_street2 = v),
    ("AlternateContactAddressCity", |r, v| r.alt_contact_address_city = v),
    ("AlternateContactAddressState", |r, v| r.alt_contact_address_state = v),
    ("AlternateContactAddressPostalCode", |r, v| r.alt_contact_address_postal_code = v),
];

/// Builds one `TenantRecord` per data row in `doc`. Errors if a required
/// column (`FirtLast` — the grouping key, with no fallback) is absent;
/// every other column is optional and defaults to blank when missing,
/// same tolerance the reference script has via `dict.get(field, "")`.
pub fn records_from_csv_document(doc: &CsvDocument) -> Result<Vec<TenantRecord>> {
    doc.header_index("FirtLast")
        .context("QMS export is missing the required FirtLast column")?;

    let resolved: Vec<(usize, ColumnSetter)> = COLUMNS
        .iter()
        .filter_map(|(header, setter)| {
            doc.header_index(header).map(|idx| (idx, *setter))
        })
        .collect();

    Ok(doc
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
