//! File ingestion parsers, one per format: CSV, OOXML/binary Excel (via
//! calamine), and hand-rolled Excel 2003 SpreadsheetML XML. Split into
//! per-format submodules since each is a genuinely independent concern;
//! this file owns only the dispatch and stays deliberately thin.
//!
//! External callers only ever need `parse_document` (the dispatch entry
//! point) or, in tests, the format-specific `parse_*` functions directly —
//! both are re-exported here so callers don't need to know about the
//! `csv`/`excel`/`spreadsheetml` submodule split underneath.

mod csv;
mod excel;
mod spreadsheetml;

pub use csv::parse_csv_document;
pub use excel::parse_excel_document;
pub use spreadsheetml::parse_spreadsheetml_document;

use spreadsheetml::is_spreadsheetml;

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

#[cfg(test)]
mod csv_tests;

#[cfg(test)]
mod excel_tests;

#[cfg(test)]
mod dispatch_tests;
