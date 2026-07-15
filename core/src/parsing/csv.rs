use std::io::Cursor;

use crate::csv_document::CsvDocument;
use crate::uploaded_file::UploadedFile;

pub fn parse_csv_document(
    file: &UploadedFile,
) -> anyhow::Result<CsvDocument> {
    let cursor =
        Cursor::new(&file.bytes);

    // `flexible(true)`: some facility export tools emit a trailing empty
    // column on every data row that the header doesn't name (confirmed
    // on real production QMS exports, not hypothetical). The strict
    // default rejects any row whose field count doesn't match the
    // header, which would reject every single row in those files.
    // Ragged rows are normalized below to exactly `headers.len()`
    // fields — the same tolerant handling the `duplicate-tenant-check`
    // reference script already relies on for this same data.
    let mut reader =
        csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(cursor);

    let headers: Vec<String> = reader
        .headers()?
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();

    let mut rows: Vec<Vec<String>> =
        Vec::new();

    for result in reader.records() {
        let record = result?;

        let mut row: Vec<String> = record
            .iter()
            .map(|field| {
                field.trim().to_string()
            })
            .collect();

        // Extra trailing fields are dropped; short rows are padded —
        // matches the reference script's `raw[:len(header)]` /
        // pad-short handling exactly.
        row.resize(headers.len(), String::new());

        rows.push(row);
    }

    Ok(CsvDocument {
        file_name:
            file.file_name.clone(),
        headers,
        rows,
    })
}
