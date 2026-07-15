use std::io::Cursor;

use crate::csv_document::CsvDocument;
use crate::uploaded_file::UploadedFile;

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
