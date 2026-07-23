use std::io::Cursor;

use calamine::{
    open_workbook_auto_from_rs,
    Data,
    Reader,
};

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
pub fn parse_excel_document(
    file: &UploadedFile,
) -> anyhow::Result<CsvDocument> {
    // `Cursor<&[u8]>` satisfies calamine's `Read + Seek` requirement just
    // as well as an owned `Cursor<Vec<u8>>` — no need to clone the whole
    // file's bytes just to hand them to the workbook reader.
    let cursor =
        Cursor::new(&file.bytes);

    let mut workbook =
        open_workbook_auto_from_rs(cursor)?;

    let sheet_names =
        workbook.sheet_names().to_vec();

    let first_sheet =
        sheet_names
            .first()
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Workbook '{}' contains no worksheets",
                    file.file_name
                )
            })?;

    let range =
        workbook.worksheet_range(
            &first_sheet,
        )?;

    let mut rows_iter =
        range.rows();

    let header_row =
        rows_iter.next().ok_or_else(|| {
            anyhow::anyhow!(
                "Workbook '{}' contains no rows",
                file.file_name
            )
        })?;

    let headers: Vec<String> =
        header_row
            .iter()
            .map(cell_to_string)
            .map(|v| {
                v.trim()
                    .to_lowercase()
            })
            .collect();

    let mut rows: Vec<Vec<String>> =
        Vec::new();

    for row in rows_iter {
        let values: Vec<String> =
            row.iter()
                .map(cell_to_string)
                .collect();

        let has_data =
            values.iter().any(|v| {
                !v.trim().is_empty()
            });

        if has_data {
            rows.push(values);
        }
    }

    Ok(CsvDocument {
        file_name:
            file.file_name.clone(),
        headers,
        rows,
        modified_at: file.modified_at,
    })
}

fn cell_to_string(
    cell: &Data,
) -> String {
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

        Data::DateTime(v) => {
            v.to_string()
        }

        Data::DateTimeIso(v) => {
            v.clone()
        }

        Data::DurationIso(v) => {
            v.clone()
        }

        Data::Error(v) => {
            v.to_string()
        }
    }
}
