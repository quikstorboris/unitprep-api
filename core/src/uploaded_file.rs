// UploadedFile represents a file exactly as received from the browser.
// No filesystem paths exist here — only the file name, its relative path
// within the uploaded folder structure, and the raw bytes.
//
// This model is intentionally source-agnostic. The same struct will be
// used regardless of whether files arrive via browser upload, SharePoint,
// OneDrive, Azure Blob, or any other future ingestion source.
// Adding a new source means implementing ingestion — not changing this model.

#[derive(Debug, Clone)]
pub struct UploadedFile {
    /// The file name only, e.g. "Mandeville_Self_Storage_Units_For_Import.csv"
    pub file_name: String,

    /// Path relative to the uploaded folder root, e.g. "Mandeville/units.csv"
    /// Preserved for output folder reconstruction. Never an absolute system path.
    ///
    /// Not read anywhere yet — output folder reconstruction isn't built. Kept
    /// (not deleted) as a placeholder for that future feature, same as `ai/`.
    #[allow(dead_code)]
    pub relative_path: String,

    /// Raw file bytes as received from the browser.
    /// Passed to the CSV parser during ingestion and not retained after CsvDocument is built.
    pub bytes: Vec<u8>,

    /// The original file's last-modified time (`File.lastModified`, epoch
    /// milliseconds), threaded through from the browser via an upload
    /// sidecar field rather than any server-side receipt time. `None` when
    /// the sidecar didn't include this file (older clients, tests). Used to
    /// tell apart multiple candidate pulls of the same facility's unit list
    /// discovered in one session — see `unit-group`'s format-resolution flow.
    pub modified_at: Option<i64>,
}