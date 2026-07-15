// CsvDocument is the parsed, in-memory representation of an UploadedFile
// that contains CSV data.
//
// PARSE ONCE POLICY: Files are parsed exactly once during upload ingestion
// and stored as CsvDocument objects in the session. All downstream processing
// (discovery, validation, analysis, export) reads from CsvDocument.
// No endpoint re-reads or re-parses files from disk or bytes after this point.
//
// This is the canonical domain representation of tabular data regardless
// of its origin. Cloud-sourced files (SharePoint, Azure Blob, OneDrive)
// will be normalized into CsvDocument before entering the processing pipeline,
// keeping all business logic source-agnostic.

#[derive(Debug, Clone)]
pub struct CsvDocument {
    /// The originating file name, preserved for error messages and output naming.
    pub file_name: String,

    /// Column headers from the first row of the CSV, normalized for comparison.
    pub headers: Vec<String>,

    /// All data rows. Each row is a Vec<String> aligned to headers by index.
    pub rows: Vec<Vec<String>>,
}

impl CsvDocument {
    /// Returns the index of a header by name, case-insensitive.
    /// Used by discovery and validation to locate columns like "UnitGroup" or "Number"
    /// without requiring callers to know the exact casing in each file.
    pub fn header_index(&self, name: &str) -> Option<usize> {
        let target = name.to_lowercase();
        self.headers
            .iter()
            .position(|h| h.to_lowercase() == target)
    }
}