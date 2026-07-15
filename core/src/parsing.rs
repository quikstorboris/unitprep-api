use std::io::Cursor;

use calamine::{
    open_workbook_auto_from_rs,
    Data,
    Reader,
};
use quick_xml::events::{
    BytesStart,
    Event,
};

use crate::csv_document::CsvDocument;
use crate::uploaded_file::UploadedFile;

/// Returns the lowercased file extension only (the text after the last
/// `.`), not the whole path — used for dispatch and for the "unsupported
/// file type" diagnostic below.
fn extension_of(file_name: &str) -> &str {
    file_name
        .rsplit('.')
        .next()
        .unwrap_or("")
}

pub fn parse_document(
    file: &UploadedFile,
) -> anyhow::Result<CsvDocument> {
    // Content is sniffed before the extension is trusted: some facility
    // export tools label Excel 2003 SpreadsheetML XML with a `.xls`
    // extension (Excel itself opens it by content, not extension), which
    // otherwise defeats calamine's binary/OOXML auto-detection. Discovery
    // decides relevance by header inspection, not by which files happened
    // to parse, so a file in this dialect needs to actually be read.
    if is_spreadsheetml(&file.bytes) {
        return parse_spreadsheetml_document(
            file,
        );
    }

    let lower =
        file.file_name.to_lowercase();

    match extension_of(&lower) {
        "csv" => parse_csv_document(file),

        "xlsx" | "xls" => {
            parse_excel_document(file)
        }

        other => {
            tracing::warn!(
                file = %file.file_name,
                extension = %other,
                "Unsupported file type — skipping"
            );

            anyhow::bail!(
                "Unsupported file type: {}",
                file.file_name
            );
        }
    }
}

pub fn parse_csv_document(
    file: &UploadedFile,
) -> anyhow::Result<CsvDocument> {
    let cursor =
        Cursor::new(&file.bytes);

    let mut reader =
        csv::Reader::from_reader(cursor);

    let headers: Vec<String> = reader
        .headers()?
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();

    let mut rows: Vec<Vec<String>> =
        Vec::new();

    for result in reader.records() {
        let record = result?;

        let row: Vec<String> = record
            .iter()
            .map(|field| {
                field.trim().to_string()
            })
            .collect();

        rows.push(row);
    }

    Ok(CsvDocument {
        file_name:
            file.file_name.clone(),
        headers,
        rows,
    })
}

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
    })
}

/// True if `bytes` looks like Excel 2003 SpreadsheetML XML — an
/// `<?xml ...?>` prolog followed by the `urn:schemas-microsoft-com:office:
/// spreadsheet` namespace within the first kilobyte. Cheap enough to run
/// on every upload regardless of extension.
fn is_spreadsheetml(
    bytes: &[u8],
) -> bool {
    let head_len =
        bytes.len().min(1024);

    let head = String::from_utf8_lossy(
        &bytes[..head_len],
    );

    let head = head
        .trim_start_matches('\u{feff}')
        .trim_start();

    head.starts_with("<?xml")
        && head.contains(
            "urn:schemas-microsoft-com:office:spreadsheet",
        )
}

fn xml_attr(
    element: &BytesStart,
    name: &[u8],
) -> Option<String> {
    element
        .attributes()
        .flatten()
        .find(|attr| {
            attr.key.as_ref() == name
        })
        .and_then(|attr| {
            attr.unescape_value().ok()
        })
        .map(|v| v.into_owned())
}

/// Places `value` into `row` at `index` (1-based, as SpreadsheetML numbers
/// columns), padding any skipped columns with empty strings, then reserves
/// `merge_across` additional empty columns after it (a `ss:MergeAcross`
/// cell visually spans those columns but carries no data of its own).
/// Advances `next_col` to the 1-based column the *next* cell without an
/// explicit `ss:Index` should land on.
fn place_spreadsheetml_cell(
    row: &mut Vec<String>,
    next_col: &mut usize,
    index: Option<usize>,
    value: String,
    merge_across: usize,
) {
    let col = index.unwrap_or(*next_col);

    if row.len() < col {
        row.resize(col, String::new());
    }

    row[col - 1] = value;

    let end_col = col + merge_across;

    if row.len() < end_col {
        row.resize(end_col, String::new());
    }

    *next_col = end_col + 1;
}

/// Parses Excel 2003 SpreadsheetML XML (`Workbook > Worksheet > Table >
/// Row > Cell > Data`) into the same CsvDocument shape as the CSV/OOXML
/// paths.
///
/// LIMITATION: only the first `<Worksheet>` is read (same limitation as
/// `parse_excel_document`). `ss:Index` gaps and `ss:MergeAcross` spans are
/// filled with empty strings; `ss:Repeat`-compressed repeated cells are
/// not expanded — not yet needed by any known export format.
pub fn parse_spreadsheetml_document(
    file: &UploadedFile,
) -> anyhow::Result<CsvDocument> {
    let text = String::from_utf8_lossy(
        &file.bytes,
    );

    let mut reader =
        quick_xml::Reader::from_str(
            &text,
        );

    let mut in_first_worksheet = false;
    let mut seen_a_worksheet = false;
    let mut finished = false;

    let mut rows: Vec<Vec<String>> =
        Vec::new();

    let mut current_row: Vec<String> =
        Vec::new();

    let mut next_col: usize = 1;

    let mut in_data = false;
    let mut cell_text = String::new();
    let mut cell_index: Option<usize> =
        None;
    let mut cell_merge_across: usize = 0;

    loop {
        let event =
            reader.read_event().map_err(
                |err| {
                    anyhow::anyhow!(
                        "Failed parsing SpreadsheetML in '{}': {}",
                        file.file_name,
                        err
                    )
                },
            )?;

        match event {
            Event::Eof => break,

            Event::Start(e) => {
                match e.name().as_ref() {
                    b"Worksheet"
                        if !seen_a_worksheet =>
                    {
                        seen_a_worksheet =
                            true;
                        in_first_worksheet =
                            true;
                    }

                    b"Row"
                        if in_first_worksheet =>
                    {
                        current_row =
                            Vec::new();
                        next_col = 1;
                    }

                    b"Cell"
                        if in_first_worksheet =>
                    {
                        cell_index = xml_attr(
                            &e, b"ss:Index",
                        )
                        .and_then(|v| {
                            v.parse().ok()
                        });

                        cell_merge_across =
                            xml_attr(
                                &e,
                                b"ss:MergeAcross",
                            )
                            .and_then(|v| {
                                v.parse().ok()
                            })
                            .unwrap_or(0);

                        cell_text =
                            String::new();
                    }

                    b"Data"
                        if in_first_worksheet =>
                    {
                        in_data = true;
                        cell_text =
                            String::new();
                    }

                    _ => {}
                }
            }

            Event::Empty(e) => {
                if in_first_worksheet
                    && e.name().as_ref()
                        == b"Cell"
                {
                    let index = xml_attr(
                        &e, b"ss:Index",
                    )
                    .and_then(|v| {
                        v.parse().ok()
                    });

                    let merge_across =
                        xml_attr(
                            &e,
                            b"ss:MergeAcross",
                        )
                        .and_then(|v| {
                            v.parse().ok()
                        })
                        .unwrap_or(0);

                    place_spreadsheetml_cell(
                        &mut current_row,
                        &mut next_col,
                        index,
                        String::new(),
                        merge_across,
                    );
                }
            }

            Event::Text(t) => {
                if in_data {
                    cell_text.push_str(
                        &t.unescape()
                            .unwrap_or_default(),
                    );
                }
            }

            Event::End(e) => {
                match e.name().as_ref() {
                    b"Data"
                        if in_first_worksheet =>
                    {
                        in_data = false;
                    }

                    b"Cell"
                        if in_first_worksheet =>
                    {
                        place_spreadsheetml_cell(
                            &mut current_row,
                            &mut next_col,
                            cell_index.take(),
                            std::mem::take(
                                &mut cell_text,
                            ),
                            cell_merge_across,
                        );

                        cell_merge_across = 0;
                    }

                    b"Row"
                        if in_first_worksheet =>
                    {
                        rows.push(
                            std::mem::take(
                                &mut current_row,
                            ),
                        );
                    }

                    b"Worksheet"
                        if in_first_worksheet =>
                    {
                        in_first_worksheet =
                            false;
                        finished = true;
                    }

                    _ => {}
                }
            }

            _ => {}
        }

        if finished {
            break;
        }
    }

    let mut rows_iter = rows.into_iter();

    let headers: Vec<String> = rows_iter
        .next()
        .ok_or_else(|| {
            anyhow::anyhow!(
                "SpreadsheetML workbook '{}' contains no rows",
                file.file_name
            )
        })?
        .into_iter()
        .map(|v| v.trim().to_lowercase())
        .collect();

    let rows: Vec<Vec<String>> = rows_iter
        .filter(|row| {
            row.iter().any(|v| {
                !v.trim().is_empty()
            })
        })
        .collect();

    Ok(CsvDocument {
        file_name:
            file.file_name.clone(),
        headers,
        rows,
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

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SPREADSHEETML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<?mso-application progid="Excel.Sheet"?>
<Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet" xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet">
 <Worksheet ss:Name="Sheet1">
  <Table>
   <Row>
    <Cell><Data ss:Type="String">Number</Data></Cell>
    <Cell><Data ss:Type="String">UnitGroup</Data></Cell>
    <Cell ss:Index="4"><Data ss:Type="String">Width</Data></Cell>
   </Row>
   <Row>
    <Cell><Data ss:Type="String">A01</Data></Cell>
    <Cell><Data ss:Type="String">10x10 Inside Climate</Data></Cell>
    <Cell ss:Index="4"><Data ss:Type="String">10</Data></Cell>
   </Row>
   <Row>
    <Cell ss:MergeAcross="1"><Data ss:Type="String">A02</Data></Cell>
    <Cell><Data ss:Type="String">10x10 Inside Climate</Data></Cell>
   </Row>
  </Table>
 </Worksheet>
</Workbook>
"#;

    fn file_with(
        name: &str,
        contents: &str,
    ) -> UploadedFile {
        UploadedFile {
            file_name: name.to_string(),
            relative_path: name
                .to_string(),
            bytes: contents
                .as_bytes()
                .to_vec(),
        }
    }

    #[test]
    fn detects_spreadsheetml_by_content_not_extension(
    ) {
        assert!(is_spreadsheetml(
            SAMPLE_SPREADSHEETML
                .as_bytes()
        ));

        assert!(!is_spreadsheetml(
            b"Number,UnitGroup\nA01,10x10 Inside Climate\n"
        ));
    }

    #[test]
    fn parses_headers_and_rows_with_index_gaps(
    ) {
        let doc =
            parse_spreadsheetml_document(
                &file_with(
                    "companySummary.xls",
                    SAMPLE_SPREADSHEETML,
                ),
            )
            .unwrap();

        assert_eq!(
            doc.headers,
            vec![
                "number",
                "unitgroup",
                "",
                "width",
            ]
        );

        assert_eq!(
            doc.rows[0],
            vec![
                "A01",
                "10x10 Inside Climate",
                "",
                "10",
            ]
        );
    }

    #[test]
    fn merge_across_reserves_spanned_columns(
    ) {
        let doc =
            parse_spreadsheetml_document(
                &file_with(
                    "companySummary.xls",
                    SAMPLE_SPREADSHEETML,
                ),
            )
            .unwrap();

        // Row 3: a MergeAcross="1" cell (spans 2 columns) followed by a
        // plain cell — the plain cell must land after the merged span,
        // not immediately next to it.
        assert_eq!(
            doc.rows[1],
            vec![
                "A02",
                "",
                "10x10 Inside Climate",
            ]
        );
    }

    #[test]
    fn parse_document_routes_xls_extension_spreadsheetml_content_correctly(
    ) {
        // A file extension of .xls that is actually SpreadsheetML XML
        // (the real-world case this parser exists for) must be content-
        // sniffed and parsed, not handed to the binary/OOXML Excel reader.
        let doc = parse_document(
            &file_with(
                "companySummary.xls",
                SAMPLE_SPREADSHEETML,
            ),
        )
        .unwrap();

        assert_eq!(
            doc.headers,
            vec![
                "number",
                "unitgroup",
                "",
                "width",
            ]
        );
    }
}
