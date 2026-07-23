use crate::parsing::parse_csv_document;
use crate::uploaded_file::UploadedFile;

#[test]
fn csv_parser_normalizes_headers() {
    let file = UploadedFile {
        file_name: "test.csv".to_string(),
        relative_path: String::new(),
        bytes: b"Number,UnitGroup\nA01,10x10 Climate\n"
            .to_vec(),
        modified_at: None,
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
fn csv_parser_trims_values() {
    let file = UploadedFile {
        file_name: "test.csv".to_string(),
        relative_path: String::new(),
        bytes: b"Number,UnitGroup\n A01 , 10x10 Climate \n"
            .to_vec(),
        modified_at: None,
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
        modified_at: None,
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
        modified_at: None,
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
        modified_at: None,
    };

    let document =
        parse_csv_document(&file)
            .unwrap();

    assert_eq!(
        document.rows[0],
        vec!["A01", "10x10 Climate", ""]
    );
}
