use std::io::Cursor;

use crate::csv_document::CsvDocument;
use crate::uploaded_file::UploadedFile;

/// Decodes raw file bytes to UTF-8 text, tolerating non-UTF-8 input.
/// Real facility exports are frequently saved by Excel as Windows-1252,
/// not true UTF-8 -- smart quotes, accented names, and em/en dashes are
/// all valid Windows-1252 but not valid UTF-8, which otherwise rejects
/// the entire file over a single character deep in a name field (a real
/// case, not hypothetical: byte 0x92 in "O'Brien" typed with a curly
/// apostrophe). Valid UTF-8 is used as-is; anything else is decoded as
/// Windows-1252, the encoding these exports actually use in practice.
fn decode_to_utf8(file_name: &str, bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(err) => {
            let (text, _, had_replacements) = encoding_rs::WINDOWS_1252.decode(bytes);
            tracing::warn!(
                file = %file_name,
                utf8_error = %err,
                had_replacements,
                "CSV file was not valid UTF-8 -- decoded as Windows-1252 instead"
            );
            text.into_owned()
        }
    }
}

pub fn parse_csv_document(file: &UploadedFile) -> anyhow::Result<CsvDocument> {
    let text = decode_to_utf8(&file.file_name, &file.bytes);
    let cursor = Cursor::new(text.as_bytes());

    // `flexible(true)`: some facility export tools emit a trailing empty
    // column on every data row that the header doesn't name (confirmed
    // on real production QMS exports, not hypothetical). The strict
    // default rejects any row whose field count doesn't match the
    // header, which would reject every single row in those files.
    // Ragged rows are normalized below to exactly `headers.len()`
    // fields — the same tolerant handling the `duplicate-tenant-check`
    // reference script already relies on for this same data.
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(cursor);

    let headers: Vec<String> = reader
        .headers()?
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();

    let mut rows: Vec<Vec<String>> = Vec::new();

    for result in reader.records() {
        let record = result?;

        let mut row: Vec<String> = record
            .iter()
            .map(|field| field.trim().to_string())
            .collect();

        // Extra trailing fields are dropped; short rows are padded —
        // matches the reference script's `raw[:len(header)]` /
        // pad-short handling exactly.
        row.resize(headers.len(), String::new());

        rows.push(row);
    }

    Ok(CsvDocument {
        file_name: file.file_name.clone(),
        headers,
        rows,
        modified_at: file.modified_at,
    })
}
