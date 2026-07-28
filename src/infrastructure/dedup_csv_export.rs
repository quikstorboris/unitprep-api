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
use crate::infrastructure::dedup_export_plan::{
    build_export_plan, record_field_values, PlannedRow, COLUMNS, NOTE_COLUMN_INDEX,
};

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
    let mut values = record_field_values(record);
    values[NOTE_COLUMN_INDEX] = note;
    values.into_iter().map(sanitize_cell).collect()
}

#[cfg(test)]
#[path = "dedup_csv_export_tests.rs"]
mod tests;
