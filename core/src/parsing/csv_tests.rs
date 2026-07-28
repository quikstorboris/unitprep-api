use proptest::prelude::*;

use crate::parsing::parse_csv_document;
use crate::uploaded_file::UploadedFile;

#[test]
fn csv_parser_normalizes_headers() {
    let file = UploadedFile {
        file_name: "test.csv".to_string(),
        relative_path: String::new(),
        bytes: b"Number,UnitGroup\nA01,10x10 Climate\n".to_vec(),
        modified_at: None,
    };

    let document = parse_csv_document(&file).unwrap();

    assert_eq!(document.headers, vec!["number", "unitgroup",]);

    assert_eq!(document.rows.len(), 1);

    assert_eq!(document.rows[0][0], "A01");

    assert_eq!(document.rows[0][1], "10x10 Climate");
}

#[test]
fn csv_parser_trims_values() {
    let file = UploadedFile {
        file_name: "test.csv".to_string(),
        relative_path: String::new(),
        bytes: b"Number,UnitGroup\n A01 , 10x10 Climate \n".to_vec(),
        modified_at: None,
    };

    let document = parse_csv_document(&file).unwrap();

    assert_eq!(document.rows[0][0], "A01");

    assert_eq!(document.rows[0][1], "10x10 Climate");
}

#[test]
fn csv_parser_preserves_leading_zeroes() {
    let file = UploadedFile {
        file_name: "test.csv".to_string(),
        relative_path: String::new(),
        bytes: b"Number,UnitGroup\n0001,10x10 Climate\n0002,10x10 Climate\n".to_vec(),
        modified_at: None,
    };

    let document = parse_csv_document(&file).unwrap();

    assert_eq!(document.rows[0][0], "0001");

    assert_eq!(document.rows[1][0], "0002");
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
        bytes: b"Number,UnitGroup\nA01,10x10 Climate,\nA02,10x10 Climate,\n".to_vec(),
        modified_at: None,
    };

    let document = parse_csv_document(&file).unwrap();

    assert_eq!(document.rows.len(), 2);
    assert_eq!(document.rows[0], vec!["A01", "10x10 Climate"]);
    assert_eq!(document.rows[1], vec!["A02", "10x10 Climate"]);
}

#[test]
fn csv_parser_pads_short_rows() {
    let file = UploadedFile {
        file_name: "test.csv".to_string(),
        relative_path: String::new(),
        bytes: b"Number,UnitGroup,Notes\nA01,10x10 Climate\n".to_vec(),
        modified_at: None,
    };

    let document = parse_csv_document(&file).unwrap();

    assert_eq!(document.rows[0], vec!["A01", "10x10 Climate", ""]);
}

fn file_with_bytes(bytes: Vec<u8>) -> UploadedFile {
    UploadedFile {
        file_name: "fuzz.csv".to_string(),
        relative_path: String::new(),
        bytes,
        modified_at: None,
    }
}

proptest! {
    /// Regardless of how ragged the input rows are (some too short, some
    /// too long, some exactly matching), every parsed row must come out
    /// exactly `headers.len()` fields wide -- the resize/pad invariant
    /// this parser exists to guarantee for every downstream consumer.
    #[test]
    fn every_row_is_padded_or_truncated_to_header_width(
        header_count in 1usize..8,
        row_field_counts in proptest::collection::vec(0usize..12, 0..20),
        field_value in "[a-zA-Z0-9 ,\"\\n]{0,12}",
    ) {
        let headers: Vec<String> = (0..header_count).map(|i| format!("h{i}")).collect();

        let mut writer = csv::WriterBuilder::new().flexible(true).from_writer(vec![]);
        writer.write_record(&headers).unwrap();
        for field_count in &row_field_counts {
            let row: Vec<&str> = (0..*field_count).map(|_| field_value.as_str()).collect();
            writer.write_record(&row).unwrap();
        }
        let bytes = writer.into_inner().unwrap();

        let document = parse_csv_document(&file_with_bytes(bytes)).unwrap();

        prop_assert_eq!(document.rows.len(), row_field_counts.len());
        for row in &document.rows {
            prop_assert_eq!(row.len(), header_count);
        }
    }

    /// Fuzz-style robustness check: parsing must never panic, no matter
    /// what bytes a client uploads under a `.csv` name -- including bytes
    /// that are not valid UTF-8 at all. A parse failure should surface as
    /// an `Err` (which callers already handle), never a crash.
    #[test]
    fn never_panics_on_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let _ = parse_csv_document(&file_with_bytes(bytes));
    }
}
