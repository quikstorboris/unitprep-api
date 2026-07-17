//! Serializes a `DedupReport`'s export plan (see `dedup_export_plan`,
//! the shared source of truth for row/column layout) to CSV bytes.
//! Kept separate from `csv_export.rs`, which is Group Prep's own
//! export artifact generation — a new tool gets its own export file
//! rather than being folded into an existing one.

use anyhow::Result;
use csv::Writer;

use unitprep_dedup::types::TenantRecord;
use unitprep_dedup::DedupReport;

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

fn record_row<'a>(record: &'a TenantRecord, note: &'a str) -> Vec<&'a str> {
    vec![
        &record.cust_numb,
        &record.unit_number,
        note,
        &record.first_last,
        &record.first_name,
        &record.last_name,
        &record.company_name,
        &record.phone_number_prefix,
        &record.phone_number,
        &record.email,
        &record.address_street1,
        &record.address_street2,
        &record.address_city,
        &record.address_state,
        &record.address_postal_code,
        &record.alt_contact_first_name,
        &record.alt_contact_last_name,
        &record.alt_contact_email,
        &record.alt_contact_phone_number_prefix,
        &record.alt_contact_phone_number,
        &record.alt_contact_address_street1,
        &record.alt_contact_address_street2,
        &record.alt_contact_address_city,
        &record.alt_contact_address_state,
        &record.alt_contact_address_postal_code,
    ]
}

#[cfg(test)]
#[path = "dedup_csv_export_tests.rs"]
mod tests;
