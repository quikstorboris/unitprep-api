use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use crate::parsing::{
    parse_csv_document,
    parse_document,
};
use crate::uploaded_file::UploadedFile;

/// Builds the smallest valid `.xlsx` workbook that satisfies the OOXML
/// spreadsheet format: one sheet, three rows, inline strings (no shared
/// string table). Built in-code with the `zip` crate (already a
/// dependency, used elsewhere for export) rather than checking in a
/// binary fixture file — this is both the input bytes and the
/// documentation of what a minimal `.xlsx` looks like.
fn minimal_xlsx_bytes() -> Vec<u8> {
    let mut cursor =
        Cursor::new(Vec::new());

    {
        let mut zip =
            ZipWriter::new(&mut cursor);

        let options =
            SimpleFileOptions::default();

        let parts: [(&str, &str); 5] = [
            (
                "[Content_Types].xml",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#,
            ),
            (
                "xl/workbook.xml",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#,
            ),
            (
                "xl/_rels/workbook.xml.rels",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#,
            ),
            (
                "xl/worksheets/sheet1.xml",
                r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<sheetData>
<row r="1"><c r="A1" t="inlineStr"><is><t>Number</t></is></c><c r="B1" t="inlineStr"><is><t>UnitGroup</t></is></c></row>
<row r="2"><c r="A2" t="inlineStr"><is><t>A01</t></is></c><c r="B2" t="inlineStr"><is><t>10x10 Inside Climate</t></is></c></row>
<row r="3"><c r="A3" t="inlineStr"><is><t>A02</t></is></c><c r="B3" t="inlineStr"><is><t>10x20 Outside Non-Climate</t></is></c></row>
</sheetData>
</worksheet>"#,
            ),
        ];

        for (path, xml) in parts {
            zip.start_file(path, options)
                .unwrap();

            zip.write_all(xml.as_bytes())
                .unwrap();
        }

        zip.finish().unwrap();
    }

    cursor.into_inner()
}

#[test]
fn csv_parser_normalizes_headers() {
    let file = UploadedFile {
        file_name: "test.csv".to_string(),
        relative_path: String::new(),
        bytes: b"Number,UnitGroup\nA01,10x10 Climate\n"
            .to_vec(),
    };

    let document =
        parse_csv_document(&file)
            .unwrap();

    assert_eq!(
        document.headers,
        vec![
            "number",
            "unitgroup",
        ]
    );

    assert_eq!(
        document.rows.len(),
        1
    );

    assert_eq!(
        document.rows[0][0],
        "A01"
    );

    assert_eq!(
        document.rows[0][1],
        "10x10 Climate"
    );
}

#[test]
fn xlsx_parser_extracts_headers_and_rows() {
    let file = UploadedFile {
        file_name: "test.xlsx".to_string(),
        relative_path: String::new(),
        bytes: minimal_xlsx_bytes(),
    };

    let document =
        parse_document(&file).unwrap();

    assert_eq!(
        document.headers,
        vec!["number", "unitgroup"]
    );

    assert_eq!(
        document.rows.len(),
        2
    );

    assert_eq!(
        document.rows[0],
        vec![
            "A01".to_string(),
            "10x10 Inside Climate"
                .to_string(),
        ]
    );

    assert_eq!(
        document.rows[1],
        vec![
            "A02".to_string(),
            "10x20 Outside Non-Climate"
                .to_string(),
        ]
    );
}

#[test]
fn xlsx_dispatch_is_case_insensitive() {
    let file = UploadedFile {
        file_name: "TEST.XLSX".to_string(),
        relative_path: String::new(),
        bytes: minimal_xlsx_bytes(),
    };

    let document =
        parse_document(&file).unwrap();

    assert_eq!(
        document.headers,
        vec!["number", "unitgroup"]
    );
}

#[test]
fn xls_extension_dispatches_to_excel_parser_not_csv() {
    // Not a real legacy .xls binary — this only proves dispatch routes
    // `.xls` to the Excel parser (which then fails on the bogus bytes)
    // rather than silently misrouting it to the CSV parser, which would
    // "succeed" by treating garbage bytes as a single malformed header row.
    let file = UploadedFile {
        file_name: "test.xls".to_string(),
        relative_path: String::new(),
        bytes: b"not a real workbook"
            .to_vec(),
    };

    let err = parse_document(&file)
        .unwrap_err();

    assert!(
        !err.to_string()
            .to_lowercase()
            .contains("unsupported"),
        "expected an Excel-parsing failure, not an unsupported-file-type error: {err}"
    );
}

#[test]
fn unsupported_file_fails() {
    let file = UploadedFile {
        file_name: "test.json".to_string(),
        relative_path: String::new(),
        bytes: b"{}".to_vec(),
    };

    assert!(
        parse_document(&file).is_err()
    );
}

#[test]
fn csv_parser_trims_values() {
    let file = UploadedFile {
        file_name: "test.csv".to_string(),
        relative_path: String::new(),
        bytes: b"Number,UnitGroup\n A01 , 10x10 Climate \n"
            .to_vec(),
    };

    let document =
        parse_csv_document(&file)
            .unwrap();

    assert_eq!(
        document.rows[0][0],
        "A01"
    );

    assert_eq!(
        document.rows[0][1],
        "10x10 Climate"
    );
}

#[test]
fn csv_parser_preserves_leading_zeroes() {
    let file = UploadedFile {
        file_name: "test.csv".to_string(),
        relative_path: String::new(),
        bytes: b"Number,UnitGroup\n0001,10x10 Climate\n0002,10x10 Climate\n"
            .to_vec(),
    };

    let document =
        parse_csv_document(&file)
            .unwrap();

    assert_eq!(
        document.rows[0][0],
        "0001"
    );

    assert_eq!(
        document.rows[1][0],
        "0002"
    );
}

#[test]
fn csv_parser_tolerates_trailing_empty_column_not_in_header() {
    // Confirmed on real QMS export files (No Ka Oi, New Castle facility
    // pulls): every data row carries one trailing empty field beyond
    // the header's last named column. The strict csv-crate default
    // rejects this as a field-count mismatch on every row.
    let file = UploadedFile {
        file_name: "test.csv".to_string(),
        relative_path: String::new(),
        bytes: b"Number,UnitGroup\nA01,10x10 Climate,\nA02,10x10 Climate,\n"
            .to_vec(),
    };

    let document =
        parse_csv_document(&file)
            .unwrap();

    assert_eq!(document.rows.len(), 2);
    assert_eq!(
        document.rows[0],
        vec!["A01", "10x10 Climate"]
    );
    assert_eq!(
        document.rows[1],
        vec!["A02", "10x10 Climate"]
    );
}

#[test]
fn csv_parser_pads_short_rows() {
    let file = UploadedFile {
        file_name: "test.csv".to_string(),
        relative_path: String::new(),
        bytes: b"Number,UnitGroup,Notes\nA01,10x10 Climate\n"
            .to_vec(),
    };

    let document =
        parse_csv_document(&file)
            .unwrap();

    assert_eq!(
        document.rows[0],
        vec!["A01", "10x10 Climate", ""]
    );
}
