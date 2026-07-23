//! Serializes a `DedupReport`'s export plan (see `dedup_export_plan`,
//! the shared source of truth for row/column layout) to CSV bytes.
//! Kept separate from `csv_export.rs`, which is Group Prep's own
//! export artifact generation — a new tool gets its own export file
//! rather than being folded into an existing one.

use anyhow::Result;
use csv::Writer;

use unitprep_dedup::types::TenantRecord;
use unitprep_dedup::DedupReport;

use crate::infrastructure::csv_safety::sanitize_cell;
use crate::infrastructure::dedup_export_plan::{build_export_plan, PlannedRow, COLUMNS};

pub fn generate_csv(report: &DedupReport, all_records: &[TenantRecord]) -> Result<Vec<u8>> {
    let plan = build_export_plan(report, all_records);

    let mut buffer = Vec::new();
    {
        let mut writer = Writer::from_writer(&mut buffer);
        writer.write_record(COLUMNS)?;

        for row in &plan {
            match row {
                PlannedRow::Data { record, note, .. } => {
                    writer.write_record(record_row(record, note))?;
                }
                PlannedRow::Blank => {
                    writer.write_record(std::iter::repeat_n("", COLUMNS.len()))?;
                }
                PlannedRow::Marker(text) => {
                    // Markers are fixed app-defined strings, never derived
                    // from uploaded data, so they don't need sanitizing.
                    let mut row = vec![*text];
                    row.extend(std::iter::repeat_n("", COLUMNS.len() - 1));
                    writer.write_record(row)?;
                }
            }
        }

        writer.flush()?;
    }
    Ok(buffer)
}

fn record_row(record: &TenantRecord, note: &str) -> Vec<String> {
    [
        record.cust_numb.as_str(),
        record.unit_number.as_str(),
        note,
        record.first_last.as_str(),
        record.first_name.as_str(),
        record.last_name.as_str(),
        record.company_name.as_str(),
        record.phone_number_prefix.as_str(),
        record.phone_number.as_str(),
        record.email.as_str(),
        record.address_street1.as_str(),
        record.address_street2.as_str(),
        record.address_city.as_str(),
        record.address_state.as_str(),
        record.address_postal_code.as_str(),
        record.alt_contact_first_name.as_str(),
        record.alt_contact_last_name.as_str(),
        record.alt_contact_email.as_str(),
        record.alt_contact_phone_number_prefix.as_str(),
        record.alt_contact_phone_number.as_str(),
        record.alt_contact_address_street1.as_str(),
        record.alt_contact_address_street2.as_str(),
        record.alt_contact_address_city.as_str(),
        record.alt_contact_address_state.as_str(),
        record.alt_contact_address_postal_code.as_str(),
    ]
    .into_iter()
    .map(sanitize_cell)
    .collect()
}

#[cfg(test)]
#[path = "dedup_csv_export_tests.rs"]
mod tests;
