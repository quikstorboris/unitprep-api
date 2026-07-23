//! Serializes a `DedupReport`'s export plan (see `dedup_export_plan`,
//! the shared source of truth for row/column layout) to a real .xlsx
//! workbook — same content and row order as the CSV export, plus two
//! things plain text can't do: auto-fit column widths (no more
//! manually dragging columns open in Excel), and a background color
//! per group/candidate cluster so adjacent findings are easy to tell
//! apart at a glance. Correction notes that cite a specific cell (see
//! `dedup_export_plan::cell_refs`) become a clickable internal
//! hyperlink jumping to it.

use anyhow::Result;
use rust_xlsxwriter::{Color, Format, Url, Workbook};

use unitprep_dedup::types::TenantRecord;
use unitprep_dedup::DedupReport;

use crate::infrastructure::csv_safety::sanitize_cell;
use crate::infrastructure::dedup_export_plan::{build_export_plan, PlannedRow, COLUMNS};

const SHEET_NAME: &str = "Duplicate Tenant Check";

/// Cycled per cluster (group/candidate), not globally unique — the
/// point is that *adjacent* clusters are visually distinct, not that
/// every cluster in a large file gets its own color, which would stop
/// being meaningfully distinguishable well before a few dozen clusters
/// anyway. Light, muted fills so the black text stays easy to read.
const CLUSTER_COLORS: &[u32] = &[0xDDEBF7, 0xE2EFDA, 0xFFF2CC, 0xFCE4D6];

const NOTE_COLUMN: u16 = 2;

pub fn generate_xlsx(report: &DedupReport, all_records: &[TenantRecord]) -> Result<Vec<u8>> {
    let plan = build_export_plan(report, all_records);

    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    worksheet.set_name(SHEET_NAME)?;

    let header_format = Format::new().set_bold();
    for (col, name) in COLUMNS.iter().enumerate() {
        worksheet.write_string_with_format(0, col as u16, *name, &header_format)?;
    }

    for (excel_row, planned_row) in (1u32..).zip(plan.iter()) {
        match planned_row {
            PlannedRow::Blank => {}
            PlannedRow::Marker(text) => {
                worksheet.write_string_with_format(excel_row, 0, *text, &Format::new().set_bold())?;
            }
            PlannedRow::Data { record, note, cluster, hyperlink_target } => {
                let format =
                    Format::new().set_background_color(Color::RGB(CLUSTER_COLORS[cluster % CLUSTER_COLORS.len()]));

                for (col, value) in record_values(record).into_iter().enumerate() {
                    if col as u16 != NOTE_COLUMN {
                        worksheet.write_string_with_format(excel_row, col as u16, value.as_str(), &format)?;
                    }
                }

                let note = sanitize_cell(note);
                write_note_cell(worksheet, excel_row, &note, hyperlink_target.as_deref(), &format)?;
            }
        }
    }

    worksheet.autofit();

    Ok(workbook.save_to_buffer()?)
}

fn write_note_cell(
    worksheet: &mut rust_xlsxwriter::Worksheet,
    row: u32,
    note: &str,
    hyperlink_target: Option<&str>,
    format: &Format,
) -> Result<()> {
    if note.is_empty() {
        worksheet.write_string_with_format(row, NOTE_COLUMN, "", format)?;
        return Ok(());
    }

    if let Some(target) = hyperlink_target {
        let url = Url::new(format!("internal:'{SHEET_NAME}'!{target}")).set_text(note);
        worksheet.write_url_with_format(row, NOTE_COLUMN, url, format)?;
    } else {
        worksheet.write_string_with_format(row, NOTE_COLUMN, note, format)?;
    }

    Ok(())
}

/// Same 25-column layout as `dedup_csv_export::record_row`, with an
/// empty placeholder at the `CorrectionNote` position — that column is
/// always written separately via `write_note_cell`, since it may need
/// to become a hyperlink rather than a plain string.
fn record_values(record: &TenantRecord) -> [String; 25] {
    [
        record.cust_numb.as_str(),
        record.unit_number.as_str(),
        "",
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
    .map(sanitize_cell)
}

#[cfg(test)]
#[path = "dedup_xlsx_export_tests.rs"]
mod tests;
