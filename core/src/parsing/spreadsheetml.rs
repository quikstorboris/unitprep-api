use quick_xml::events::{
    BytesStart,
    Event,
};

use crate::csv_document::CsvDocument;
use crate::uploaded_file::UploadedFile;

/// True if `bytes` looks like Excel 2003 SpreadsheetML XML — an
/// `<?xml ...?>` prolog followed by the `urn:schemas-microsoft-com:office:
/// spreadsheet` namespace within the first kilobyte. Cheap enough to run
/// on every upload regardless of extension. `pub(crate)` — only
/// `parsing::parse_document`'s dispatch needs this; parsing the actual
/// content stays entirely within this module.
pub(crate) fn is_spreadsheetml(
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
/// `excel::parse_excel_document`). `ss:Index` gaps and `ss:MergeAcross`
/// spans are filled with empty strings; `ss:Repeat`-compressed repeated
/// cells are not expanded — not yet needed by any known export format.
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
        modified_at: file.modified_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsing::parse_document;

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
            modified_at: None,
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
