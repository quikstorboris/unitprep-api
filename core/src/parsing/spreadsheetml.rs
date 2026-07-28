use quick_xml::events::{BytesStart, Event};

use crate::csv_document::CsvDocument;
use crate::uploaded_file::UploadedFile;

/// True if `bytes` looks like Excel 2003 SpreadsheetML XML — an
/// `<?xml ...?>` prolog followed by the `urn:schemas-microsoft-com:office:
/// spreadsheet` namespace within the first kilobyte. Cheap enough to run
/// on every upload regardless of extension. `pub(crate)` — only
/// `parsing::parse_document`'s dispatch needs this; parsing the actual
/// content stays entirely within this module.
pub(crate) fn is_spreadsheetml(bytes: &[u8]) -> bool {
    let head_len = bytes.len().min(1024);

    let head = String::from_utf8_lossy(&bytes[..head_len]);

    let head = head.trim_start_matches('\u{feff}').trim_start();

    head.starts_with("<?xml") && head.contains("urn:schemas-microsoft-com:office:spreadsheet")
}

/// Excel's own real column limit (`XFD`, column 16384) — used to bound
/// `ss:Index`/`ss:MergeAcross` before they ever reach `Vec::resize`. These
/// two attributes come straight from untrusted uploaded XML with no
/// existing bound: a single crafted cell (e.g. `ss:Index="99999999999999"`)
/// would otherwise attempt an astronomical allocation and abort the whole
/// process — not a `panic!` the catch-panic middleware could intercept,
/// an allocator abort. Found via property-testing `place_spreadsheetml_cell`
/// with arbitrary usize values.
const MAX_SPREADSHEETML_COLUMN: usize = 16_384;

/// Parses a column-index-shaped XML attribute value, rejecting anything
/// outside `1..=MAX_SPREADSHEETML_COLUMN` as if the attribute were absent
/// -- `ss:Index`/`ss:MergeAcross` are 1-based per the SpreadsheetML spec
/// (a 0 would otherwise underflow `col - 1` in `place_spreadsheetml_cell`),
/// and no real spreadsheet needs more columns than Excel itself supports.
fn parse_bounded_column(value: &str) -> Option<usize> {
    value
        .parse::<usize>()
        .ok()
        .filter(|&n| (1..=MAX_SPREADSHEETML_COLUMN).contains(&n))
}

fn xml_attr(element: &BytesStart, name: &[u8]) -> Option<String> {
    element
        .attributes()
        .flatten()
        .find(|attr| attr.key.as_ref() == name)
        .and_then(|attr| {
            attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()
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
    // Defense in depth: callers are expected to have already bounded
    // `index`/`merge_across` via `parse_bounded_column` before reaching
    // here, but this function does its own clamping too rather than
    // trusting that -- an unbounded `col`/`end_col` feeding `Vec::resize`
    // is an allocator abort waiting to happen, not just a bad value.
    let col = index.unwrap_or(*next_col).min(MAX_SPREADSHEETML_COLUMN);

    if row.len() < col {
        row.resize(col, String::new());
    }

    if col >= 1 {
        row[col - 1] = value;
    }

    let end_col = col
        .saturating_add(merge_across)
        .min(MAX_SPREADSHEETML_COLUMN);

    if row.len() < end_col {
        row.resize(end_col, String::new());
    }

    *next_col = end_col.saturating_add(1);
}

/// Parses Excel 2003 SpreadsheetML XML (`Workbook > Worksheet > Table >
/// Row > Cell > Data`) into the same CsvDocument shape as the CSV/OOXML
/// paths.
///
/// LIMITATION: only the first `<Worksheet>` is read (same limitation as
/// `excel::parse_excel_document`). `ss:Index` gaps and `ss:MergeAcross`
/// spans are filled with empty strings; `ss:Repeat`-compressed repeated
/// cells are not expanded — not yet needed by any known export format.
pub fn parse_spreadsheetml_document(file: &UploadedFile) -> anyhow::Result<CsvDocument> {
    let text = String::from_utf8_lossy(&file.bytes);

    let mut reader = quick_xml::Reader::from_str(&text);

    let mut in_first_worksheet = false;
    let mut seen_a_worksheet = false;
    let mut finished = false;

    let mut rows: Vec<Vec<String>> = Vec::new();

    let mut current_row: Vec<String> = Vec::new();

    let mut next_col: usize = 1;

    let mut in_data = false;
    let mut cell_text = String::new();
    let mut cell_index: Option<usize> = None;
    let mut cell_merge_across: usize = 0;

    loop {
        let event = reader.read_event().map_err(|err| {
            anyhow::anyhow!(
                "Failed parsing SpreadsheetML in '{}': {}",
                file.file_name,
                err
            )
        })?;

        match event {
            Event::Eof => break,

            Event::Start(e) => match e.name().as_ref() {
                b"Worksheet" if !seen_a_worksheet => {
                    seen_a_worksheet = true;
                    in_first_worksheet = true;
                }

                b"Row" if in_first_worksheet => {
                    current_row = Vec::new();
                    next_col = 1;
                }

                b"Cell" if in_first_worksheet => {
                    cell_index = xml_attr(&e, b"ss:Index").and_then(|v| parse_bounded_column(&v));

                    cell_merge_across = xml_attr(&e, b"ss:MergeAcross")
                        .and_then(|v| parse_bounded_column(&v))
                        .unwrap_or(0);

                    cell_text = String::new();
                }

                b"Data" if in_first_worksheet => {
                    in_data = true;
                    cell_text = String::new();
                }

                _ => {}
            },

            Event::Empty(e) => {
                if in_first_worksheet && e.name().as_ref() == b"Cell" {
                    let index = xml_attr(&e, b"ss:Index").and_then(|v| parse_bounded_column(&v));

                    let merge_across = xml_attr(&e, b"ss:MergeAcross")
                        .and_then(|v| parse_bounded_column(&v))
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
                    // quick-xml 0.41 split what used to be one `unescape()`
                    // call into a charset-decode step and a separate
                    // entity-unescape step (`&amp;` -> `&` etc.) -- both
                    // needed to reproduce the old behavior.
                    let decoded = t.decode().unwrap_or_default();
                    let unescaped = quick_xml::escape::unescape(&decoded).unwrap_or_default();
                    cell_text.push_str(&unescaped);
                }
            }

            Event::End(e) => match e.name().as_ref() {
                b"Data" if in_first_worksheet => {
                    in_data = false;
                }

                b"Cell" if in_first_worksheet => {
                    place_spreadsheetml_cell(
                        &mut current_row,
                        &mut next_col,
                        cell_index.take(),
                        std::mem::take(&mut cell_text),
                        cell_merge_across,
                    );

                    cell_merge_across = 0;
                }

                b"Row" if in_first_worksheet => {
                    rows.push(std::mem::take(&mut current_row));
                }

                b"Worksheet" if in_first_worksheet => {
                    in_first_worksheet = false;
                    finished = true;
                }

                _ => {}
            },

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
        .filter(|row| row.iter().any(|v| !v.trim().is_empty()))
        .collect();

    Ok(CsvDocument {
        file_name: file.file_name.clone(),
        headers,
        rows,
        modified_at: file.modified_at,
    })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

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

    fn file_with(name: &str, contents: &str) -> UploadedFile {
        UploadedFile {
            file_name: name.to_string(),
            relative_path: name.to_string(),
            bytes: contents.as_bytes().to_vec(),
            modified_at: None,
        }
    }

    #[test]
    fn detects_spreadsheetml_by_content_not_extension() {
        assert!(is_spreadsheetml(SAMPLE_SPREADSHEETML.as_bytes()));

        assert!(!is_spreadsheetml(
            b"Number,UnitGroup\nA01,10x10 Inside Climate\n"
        ));
    }

    #[test]
    fn parses_headers_and_rows_with_index_gaps() {
        let doc =
            parse_spreadsheetml_document(&file_with("companySummary.xls", SAMPLE_SPREADSHEETML))
                .unwrap();

        assert_eq!(doc.headers, vec!["number", "unitgroup", "", "width",]);

        assert_eq!(doc.rows[0], vec!["A01", "10x10 Inside Climate", "", "10",]);
    }

    #[test]
    fn merge_across_reserves_spanned_columns() {
        let doc =
            parse_spreadsheetml_document(&file_with("companySummary.xls", SAMPLE_SPREADSHEETML))
                .unwrap();

        // Row 3: a MergeAcross="1" cell (spans 2 columns) followed by a
        // plain cell — the plain cell must land after the merged span,
        // not immediately next to it.
        assert_eq!(doc.rows[1], vec!["A02", "", "10x10 Inside Climate",]);
    }

    #[test]
    fn parse_document_routes_xls_extension_spreadsheetml_content_correctly() {
        // A file extension of .xls that is actually SpreadsheetML XML
        // (the real-world case this parser exists for) must be content-
        // sniffed and parsed, not handed to the binary/OOXML Excel reader.
        let doc = parse_document(&file_with("companySummary.xls", SAMPLE_SPREADSHEETML)).unwrap();

        assert_eq!(doc.headers, vec!["number", "unitgroup", "", "width",]);
    }

    /// Regression test for a crafted-file DoS this session's fuzz testing
    /// found: an absurd `ss:Index` used to reach `Vec::resize` completely
    /// unbounded. A file this small should never do more than fail to
    /// parse cleanly.
    #[test]
    fn absurd_index_attribute_does_not_abort_the_process() {
        let xml = SAMPLE_SPREADSHEETML.replace(
            r#"<Cell ss:Index="4">"#,
            r#"<Cell ss:Index="99999999999999">"#,
        );

        let doc = parse_spreadsheetml_document(&file_with("evil.xls", &xml)).unwrap();

        // The out-of-range index is treated as absent (falls back to
        // positional placement) rather than honored literally.
        assert!(doc.headers.len() <= MAX_SPREADSHEETML_COLUMN);
    }

    proptest! {
        /// `place_spreadsheetml_cell` does raw index arithmetic against a
        /// growable `Vec`, fed by two attributes parsed straight from
        /// untrusted uploaded XML. Any combination of index/merge_across
        /// -- including values right at `usize::MAX` -- must be handled
        /// without an overflow panic or an astronomical allocation.
        #[test]
        fn place_cell_never_overflows_or_allocates_unboundedly(
            index in proptest::option::of(any::<usize>()),
            merge_across in any::<usize>(),
        ) {
            let mut row: Vec<String> = Vec::new();
            let mut next_col: usize = 1;

            place_spreadsheetml_cell(&mut row, &mut next_col, index, "x".to_string(), merge_across);

            prop_assert!(row.len() <= MAX_SPREADSHEETML_COLUMN);
            prop_assert!(next_col <= MAX_SPREADSHEETML_COLUMN + 1);
        }

        /// Whole-document fuzz: parsing must never panic or hang on
        /// arbitrary bytes under a `.xls`/SpreadsheetML-shaped name,
        /// whether or not they happen to be well-formed XML.
        #[test]
        fn never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
            let file = UploadedFile {
                file_name: "fuzz.xls".to_string(),
                relative_path: String::new(),
                bytes,
                modified_at: None,
            };

            let _ = parse_spreadsheetml_document(&file);
        }
    }
}
