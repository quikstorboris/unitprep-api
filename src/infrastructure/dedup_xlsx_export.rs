//! Serializes a `DedupReport`'s export plan (see `dedup_export_plan`,
//! the shared source of truth for row/column layout) to a real .xlsx
//! workbook — same content and row order as the CSV export, plus
//! several things plain text can't do: auto-fit column widths (capped
//! and wrapped on the Note column specifically, see `NOTE_COLUMN`'s own
//! setup below), a background color per group/candidate cluster so
//! adjacent findings are easy to tell apart at a glance, a frozen
//! header row, an autofilter, and section-banner rows merged and filled
//! across the full width so they read as dividers rather than stray
//! tenant records. Correction notes that cite a specific cell (see
//! `dedup_export_plan::cell_refs`) become a clickable internal
//! hyperlink jumping to it.
//!
//! Does NOT run cell values through `csv_safety::sanitize_cell` (unlike
//! `dedup_csv_export.rs`, which must) — verified directly against
//! `rust_xlsxwriter`'s own `Worksheet::store_string` (the function every
//! `write_string*` call in this file ultimately goes through): it
//! writes an explicitly string-typed cell with zero inspection of the
//! string's leading character, so a value starting with `=`/`+`/`-`/`@`
//! is stored as literal text, never as a live formula. The CSV-injection
//! risk `sanitize_cell` guards against (CWE-1236) is specific to
//! delimited text formats, where Excel's *open* heuristic re-infers each
//! cell's type from raw text with no format metadata to trust instead —
//! a real .xlsx file carries that type metadata explicitly, so there is
//! nothing for Excel to re-infer. Applying the CSV-specific mitigation
//! here anyway bought no real safety and cost a real, reported bug: a
//! `PhoneNumberPrefix` value of `"+1"` rendered as the literal text
//! `'+1` in some viewers, since the leading apostrophe (meaningful only
//! as an Excel *CSV-import* convention) has no special meaning once
//! it's just another character in a plain string cell.

use anyhow::Result;
use rust_xlsxwriter::{Color, Format, Url, Workbook};

use unitprep_dedup::types::TenantRecord;
use unitprep_dedup::DedupReport;

use crate::infrastructure::dedup_export_plan::{
    build_export_plan, record_field_values, PlannedRow, COLUMNS, NOTE_COLUMN_INDEX,
};

const SHEET_NAME: &str = "Duplicate Tenant Check";

/// Cycled per cluster (group/candidate), not globally unique — the
/// point is that *adjacent* clusters are visually distinct, not that
/// every cluster in a large file gets its own color, which would stop
/// being meaningfully distinguishable well before a few dozen clusters
/// anyway. Light, muted fills so the black text stays easy to read.
const CLUSTER_COLORS: &[u32] = &[0xDDEBF7, 0xE2EFDA, 0xFFF2CC, 0xFCE4D6];

/// Distinct from every cluster color, and from the header's plain bold
/// (no fill) — a section banner (`PlannedRow::Marker`) must never be
/// mistakable for either a cluster row or the header. Real bug this
/// closes: an unstyled banner row read as a stray, malformed tenant
/// record rather than a section divider.
const BANNER_FILL: u32 = 0xBFBFBF;

const NOTE_COLUMN: u16 = NOTE_COLUMN_INDEX as u16;
const LAST_COLUMN: u16 = COLUMNS.len() as u16 - 1;

/// Real bug this closes: a blind whole-sheet `autofit()` sizes the Note
/// column to its longest cell — a full correction-note sentence, often
/// citing several units and cells — which can hit Excel's own 255-
/// character/1790-pixel autofit ceiling, pushing every column after it
/// off-screen with no way to see the header past the first scroll.
/// `rust_xlsxwriter`'s own docs recommend exactly this value as "a good
/// compromise between column width and readability" for this exact
/// scenario (see `Worksheet::set_autofit_max_width`'s doc comment).
const AUTOFIT_MAX_WIDTH_PIXELS: u32 = 300;

pub fn generate_xlsx(report: &DedupReport, all_records: &[TenantRecord]) -> Result<Vec<u8>> {
    let plan = build_export_plan(report, all_records);

    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    worksheet.set_name(SHEET_NAME)?;

    let header_format = Format::new().set_bold();
    for (col, name) in COLUMNS.iter().enumerate() {
        worksheet.write_string_with_format(0, col as u16, *name, &header_format)?;
    }

    let banner_format = Format::new()
        .set_bold()
        .set_background_color(Color::RGB(BANNER_FILL));

    let mut last_row = 0u32;

    for (excel_row, planned_row) in (1u32..).zip(plan.iter()) {
        last_row = excel_row;

        match planned_row {
            PlannedRow::Blank => {}
            PlannedRow::Marker(text) => {
                // Merged across every column (not just written into
                // column A) so the banner reads as one continuous
                // divider, not a one-cell label sitting in an otherwise
                // empty row that looks like a malformed data row.
                worksheet.merge_range(excel_row, 0, excel_row, LAST_COLUMN, text, &banner_format)?;
            }
            PlannedRow::Data {
                record,
                note,
                cluster,
                hyperlink_target,
            } => {
                let format = Format::new().set_background_color(Color::RGB(
                    CLUSTER_COLORS[cluster % CLUSTER_COLORS.len()],
                ));

                for (col, value) in record_values(record).into_iter().enumerate() {
                    if col as u16 != NOTE_COLUMN {
                        worksheet.write_string_with_format(
                            excel_row,
                            col as u16,
                            value.as_str(),
                            &format,
                        )?;
                    }
                }

                write_note_cell(worksheet, excel_row, note, hyperlink_target.as_deref(), &format)?;
            }
        }
    }

    worksheet.set_autofit_max_width(AUTOFIT_MAX_WIDTH_PIXELS);
    worksheet.autofit();

    // Wrap text on the Note column specifically, applied after autofit
    // so it isn't overridden by autofit's own column-width bookkeeping —
    // the capped width above only helps if the resulting overflow text
    // actually wraps within the cell instead of being visually clipped.
    worksheet.set_column_width(NOTE_COLUMN, 60)?;

    // Header stays visible while scrolling through a long report — see
    // the review finding this fixes: "the header disappears on the
    // first scroll" with no freeze panes on a 237-row sheet.
    worksheet.set_freeze_panes(1, 0)?;

    // Lets a facility manager filter/sort the export directly in Excel
    // rather than scanning the whole sheet by eye.
    worksheet.autofilter(0, 0, last_row, LAST_COLUMN)?;

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

/// The `CorrectionNote` position is already a blank placeholder in
/// `record_field_values` — that column is always written separately via
/// `write_note_cell`, since it may need to become a hyperlink rather
/// than a plain string. No `sanitize_cell` here — see this module's own
/// doc comment for why the xlsx writer doesn't need it.
fn record_values(record: &TenantRecord) -> [String; 25] {
    record_field_values(record).map(String::from)
}

#[cfg(test)]
#[path = "dedup_xlsx_export_tests.rs"]
mod tests;
