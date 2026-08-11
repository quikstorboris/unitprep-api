//! Surgical, minimal-diff text edits inside a `.docx` file.
//!
//! A `.docx` is a zip archive of XML parts. The obvious way to edit one
//! programmatically -- parse the whole document into an object model,
//! mutate it, serialize it back out -- carries real fidelity risk: any
//! OOXML detail the model doesn't fully capture (an obscure run
//! property, a content control, a comment anchor) can be silently
//! dropped or altered on the way back out, even in parts of the
//! document nobody meant to touch.
//!
//! This crate takes the opposite approach: never deserialize the
//! document into a model that claims to represent the whole thing.
//! Instead, flatten just the plain text ([`read::extract_flat_text`]),
//! let a caller work out *what* to change in that flat text, then
//! splice each change back into the **original, unmodified** XML bytes
//! at the exact byte range the matching `<w:t>` run occupied
//! ([`edit::apply_edits`]). Every byte outside a targeted run's text
//! content -- every other run, every table, every style, every other
//! part in the zip -- is copied through unchanged. There is nothing
//! else in the file that this crate is capable of altering, by
//! construction, not by care.
//!
//! Scope: this handles the OOXML most real-world `.docx` files actually
//! use (paragraphs, runs, tables-as-plain-text, tabs). It does not
//! understand content controls, tracked changes, or comments -- a
//! document using those will still flatten to readable text and edit
//! correctly, just without any special handling for what those features
//! mean.

pub mod edit;
pub mod read;

pub use edit::{apply_edits, Edit, EditError};
pub use read::{extract_flat_text, FlatText, RunSpan};

use std::io::{Cursor, Read, Write};

const DOCUMENT_XML_PATH: &str = "word/document.xml";

#[derive(Debug)]
pub enum DocxError {
    Zip(zip::result::ZipError),
    Io(std::io::Error),
    MissingDocumentXml,
    InvalidUtf8,
    Edit(EditError),
}

impl From<zip::result::ZipError> for DocxError {
    fn from(err: zip::result::ZipError) -> Self {
        DocxError::Zip(err)
    }
}

impl From<std::io::Error> for DocxError {
    fn from(err: std::io::Error) -> Self {
        DocxError::Io(err)
    }
}

/// Extracts the flattened body text of a `.docx` given as raw bytes.
/// The returned [`FlatText`] is what a caller runs pattern-matching
/// against to decide what needs to change.
pub fn read_docx(docx_bytes: &[u8]) -> Result<FlatText, DocxError> {
    let document_xml = read_document_xml(docx_bytes)?;
    Ok(extract_flat_text(&document_xml))
}

/// Applies `edits` (in the same flat-text coordinates [`read_docx`]
/// returned) to a `.docx` given as raw bytes, returning a new `.docx`'s
/// bytes. Every zip entry other than `word/document.xml` is copied
/// through byte-for-byte from the input; within `document.xml`, every
/// byte outside an edited run's text content is likewise untouched.
pub fn edit_docx(docx_bytes: &[u8], edits: &[Edit]) -> Result<Vec<u8>, DocxError> {
    let document_xml = read_document_xml(docx_bytes)?;
    let flat = extract_flat_text(&document_xml);
    let edited_xml = apply_edits(&document_xml, &flat, edits).map_err(DocxError::Edit)?;

    let mut input_zip = zip::ZipArchive::new(Cursor::new(docx_bytes))?;
    let mut output_buffer = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(Cursor::new(&mut output_buffer));

        for i in 0..input_zip.len() {
            let entry = input_zip.by_index(i)?;
            let name = entry.name().to_string();
            let options: zip::write::FileOptions<()> =
                zip::write::FileOptions::default().compression_method(entry.compression());

            if name == DOCUMENT_XML_PATH {
                drop(entry);
                writer.start_file(&name, options)?;
                writer.write_all(edited_xml.as_bytes())?;
            } else {
                let mut entry = entry;
                let mut contents = Vec::new();
                entry.read_to_end(&mut contents)?;
                writer.start_file(&name, options)?;
                writer.write_all(&contents)?;
            }
        }

        writer.finish()?;
    }

    Ok(output_buffer)
}

fn read_document_xml(docx_bytes: &[u8]) -> Result<String, DocxError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(docx_bytes))?;
    let mut entry = zip
        .by_name(DOCUMENT_XML_PATH)
        .map_err(|_| DocxError::MissingDocumentXml)?;
    let mut contents = String::new();
    entry
        .read_to_string(&mut contents)
        .map_err(|_| DocxError::InvalidUtf8)?;
    Ok(contents)
}
