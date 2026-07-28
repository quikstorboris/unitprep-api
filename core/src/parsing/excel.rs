use std::io::Cursor;

use calamine::{open_workbook_auto_from_rs, Data, Reader};
use chrono::NaiveTime;

use crate::csv_document::CsvDocument;
use crate::uploaded_file::UploadedFile;

/// Parses an Excel workbook (`.xlsx`/`.xls`) into a CsvDocument.
///
/// LIMITATION: only the first worksheet is read. If a real-world export
/// puts data on a sheet other than the first (e.g. a cover/readme sheet
/// precedes it), that data will not be found. Not yet needed by any known
/// export format, so left as a documented limitation rather than adding
/// sheet-selection UI/logic before there's a real use case for it — revisit
/// if a workbook with the data on a non-first sheet shows up.
pub fn parse_excel_document(file: &UploadedFile) -> anyhow::Result<CsvDocument> {
    // `Cursor<&[u8]>` satisfies calamine's `Read + Seek` requirement just
    // as well as an owned `Cursor<Vec<u8>>` — no need to clone the whole
    // file's bytes just to hand them to the workbook reader.
    let cursor = Cursor::new(&file.bytes);

    let mut workbook = open_workbook_auto_from_rs(cursor)?;

    let first_sheet =
        workbook.sheet_names().first().cloned().ok_or_else(|| {
            anyhow::anyhow!("Workbook '{}' contains no worksheets", file.file_name)
        })?;

    let range = workbook.worksheet_range(&first_sheet)?;

    let mut rows_iter = range.rows();

    let header_row = rows_iter
        .next()
        .ok_or_else(|| anyhow::anyhow!("Workbook '{}' contains no rows", file.file_name))?;

    let headers: Vec<String> = header_row
        .iter()
        .map(cell_to_string)
        .map(|v| v.trim().to_lowercase())
        .collect();

    let mut rows: Vec<Vec<String>> = Vec::new();

    for row in rows_iter {
        let values: Vec<String> = row.iter().map(cell_to_string).collect();

        let has_data = values.iter().any(|v| !v.trim().is_empty());

        if has_data {
            rows.push(values);
        }
    }

    Ok(CsvDocument {
        file_name: file.file_name.clone(),
        headers,
        rows,
        modified_at: file.modified_at,
    })
}

fn cell_to_string(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(v) => v.clone(),
        Data::Bool(v) => v.to_string(),
        Data::Int(v) => v.to_string(),

        Data::Float(v) => {
            if v.fract() == 0.0 {
                (*v as i64).to_string()
            } else {
                v.to_string()
            }
        }

        Data::DateTime(v) => match v.as_datetime() {
            // Cells with no time-of-day component (the common case for a
            // plain date column) render as a bare date, not
            // midnight-suffixed noise.
            Some(dt) if dt.time() == NaiveTime::MIN => dt.format("%Y-%m-%d").to_string(),
            Some(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            // `as_datetime()` can return `None` on overflow (see calamine's
            // own doc comment) — fall back to the raw serial number rather
            // than panicking or dropping the cell's value entirely.
            None => v.to_string(),
        },

        Data::DateTimeIso(v) => v.clone(),

        Data::DurationIso(v) => v.clone(),

        Data::Error(v) => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use calamine::{ExcelDateTime, ExcelDateTimeType};

    use super::*;

    #[test]
    fn date_only_cell_formats_as_plain_date() {
        // Excel serial 45292 = 2024-01-01 (no time-of-day component).
        let cell = Data::DateTime(ExcelDateTime::new(
            45292.0,
            ExcelDateTimeType::DateTime,
            false,
        ));

        assert_eq!(cell_to_string(&cell), "2024-01-01");
    }

    #[test]
    fn datetime_cell_with_time_formats_with_time() {
        // 45292.5 = 2024-01-01 12:00:00.
        let cell = Data::DateTime(ExcelDateTime::new(
            45292.5,
            ExcelDateTimeType::DateTime,
            false,
        ));

        assert_eq!(cell_to_string(&cell), "2024-01-01 12:00:00");
    }

    #[test]
    fn non_date_cell_types_are_unaffected() {
        assert_eq!(cell_to_string(&Data::Int(42)), "42");
        assert_eq!(cell_to_string(&Data::Float(10.0)), "10");
        assert_eq!(cell_to_string(&Data::String("hello".to_string())), "hello");
    }
}
