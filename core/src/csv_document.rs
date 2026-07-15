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
    /// The single normalization rule every column-name comparison in the
    /// system must use: lowercased, with spaces and underscores stripped.
    /// This exists as one shared function specifically so nothing else
    /// grows its own ad hoc normalizer that silently disagrees with it —
    /// that's exactly what happened before this existed: discovery
    /// classified a file as a unit file using a normalizer that stripped
    /// separators, while `header_index` only lowercased, so a header
    /// like `"Unit_Group"` or `"Unit Group"` could pass discovery's
    /// check and then fail every lookup validation did afterward,
    /// silently producing zero validation issues for a file discovery
    /// had just confirmed was a real unit file.
    pub fn normalize_header_name(
        name: &str,
    ) -> String {
        name.to_lowercase()
            .replace(['_', ' '], "")
    }

    /// Returns the index of a header by name — case- and separator-
    /// insensitive (see `normalize_header_name`) — so callers never need
    /// to know the exact spelling, casing, or spacing a given file uses.
    /// Used by discovery and validation to locate columns like
    /// "UnitGroup" or "Number".
    pub fn header_index(&self, name: &str) -> Option<usize> {
        let target = Self::normalize_header_name(name);
        self.headers
            .iter()
            .position(|h| {
                Self::normalize_header_name(h) == target
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_index_ignores_spaces_and_underscores() {
        let document = CsvDocument {
            file_name: "test.csv".to_string(),
            headers: vec![
                "unit_group".to_string(),
                "number".to_string(),
            ],
            rows: Vec::new(),
        };

        // A caller looking up "UnitGroup" (no separator) must still find
        // a header stored as "unit_group" — this is the exact mismatch
        // that let a discovered unit file silently produce zero
        // validation issues (discovery's normalizer stripped the
        // separator; the old `header_index` didn't).
        assert_eq!(
            document.header_index("UnitGroup"),
            Some(0)
        );
    }

    #[test]
    fn header_index_returns_none_for_absent_header() {
        let document = CsvDocument {
            file_name: "test.csv".to_string(),
            headers: vec!["number".to_string()],
            rows: Vec::new(),
        };

        assert_eq!(
            document.header_index("unitgroup"),
            None
        );
    }
}