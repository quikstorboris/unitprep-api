//! Real parsing logic for the rare vendor whose export packs several
//! canonical fields into one raw column in a shape no rename table can
//! express. Deliberately the ONLY place vendor-specific code is allowed
//! to exist — `client_ops.vendor_format` has no code column, so a
//! self-service "add a vendor" flow can never reach this module; a
//! custom vendor that turns out to need a real transform is a signal it
//! should graduate into a hand-authored row here, reviewed and tested
//! like any other code change, the same path Storage Commander and
//! DoorSwap followed before this table existed at all.

use crate::csv_document::CsvDocument;

mod ess;
mod quikstor_cloud;
mod sitelink;

pub fn apply(key: &str, document: &CsvDocument) -> anyhow::Result<CsvDocument> {
    match key {
        "split_ess_address" => Ok(ess::split_ess_address(document)),
        "derive_quikstor_cloud_tenant_fields" => Ok(
            quikstor_cloud::derive_quikstor_cloud_tenant_fields(document),
        ),
        "derive_sitelink_tenant_fields" => Ok(sitelink::derive_sitelink_tenant_fields(document)),
        "derive_sitelink_unit_group" => Ok(sitelink::derive_sitelink_unit_group(document)),
        other => anyhow::bail!("unknown vendor-format transform key: {other}"),
    }
}

/// Trimmed value of `header` in `row`, or `""` when the document has no
/// such column or the row is short. Shared by every transform that has
/// to read several optional source columns.
fn cell(document: &CsvDocument, row: &[String], header: &str) -> String {
    document
        .header_index(header)
        .and_then(|idx| row.get(idx))
        .map(|v| v.trim().to_string())
        .unwrap_or_default()
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Dedup's grouping key for exports with no single name column:
/// `"{first} {last}"` with whitespace collapsed, falling back to
/// `company` when both name halves are blank. Middle names are left out
/// on purpose so two rows for one tenant that differ only by a middle
/// name still group together.
fn person_key(first: &str, last: &str, company: &str) -> String {
    let key = collapse_whitespace(&format!("{first} {last}"));
    if key.is_empty() {
        collapse_whitespace(company)
    } else {
        key
    }
}
