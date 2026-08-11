//! Proves the "surgical" claim against a real document, not a
//! synthetic fixture -- Atherton Storage's actual blank lease template
//! (no real tenant data, safe to commit; see `tests/fixtures/`).

use docx_surgeon::{edit_docx, read_docx, Edit, RegionRef};
use std::fs;
use std::io::{Cursor, Read};
use zip::ZipArchive;

const FIXTURE: &str = "tests/fixtures/atherton-storage-contract.docx";

#[test]
fn edits_a_real_document_and_leaves_every_other_zip_entry_byte_identical() {
    let original_bytes = fs::read(FIXTURE).expect("fixture must exist");

    let flat = read_docx(&original_bytes)
        .expect("should read a real .docx")
        .body;
    let needle = "Atherton Storage";
    let occurrences_before = flat.text.matches(needle).count();
    assert!(
        occurrences_before > 1,
        "fixture assumption changed -- pick a different needle"
    );

    let start = flat
        .text
        .find(needle)
        .expect("fixture should contain the facility name");
    let end = start + needle.len();

    let edited_bytes = edit_docx(
        &original_bytes,
        &[Edit {
            region: RegionRef::Body,
            flat_start: start,
            flat_end: end,
            replacement: "{{f.name}}".to_string(),
        }],
    )
    .expect("edit should succeed against a real document");

    let flat_after = read_docx(&edited_bytes)
        .expect("edited docx should still be a valid, readable .docx")
        .body;
    assert!(flat_after.text.starts_with("{{f.name}}"));
    assert_eq!(
        flat_after.text.matches(needle).count(),
        occurrences_before - 1,
        "only the first occurrence should have changed -- the rest of the document repeats the facility name"
    );

    let mut original_zip = ZipArchive::new(Cursor::new(&original_bytes)).unwrap();
    let mut edited_zip = ZipArchive::new(Cursor::new(&edited_bytes)).unwrap();
    assert_eq!(
        original_zip.len(),
        edited_zip.len(),
        "no parts should be added or removed"
    );

    for i in 0..original_zip.len() {
        let mut orig_entry = original_zip.by_index(i).unwrap();
        let name = orig_entry.name().to_string();
        let mut orig_contents = Vec::new();
        orig_entry.read_to_end(&mut orig_contents).unwrap();

        let mut edited_entry = edited_zip.by_name(&name).unwrap();
        let mut edited_contents = Vec::new();
        edited_entry.read_to_end(&mut edited_contents).unwrap();

        if name == "word/document.xml" {
            assert_ne!(
                orig_contents, edited_contents,
                "document.xml should be the one part that changed"
            );
        } else {
            assert_eq!(
                orig_contents, edited_contents,
                "{name} is untouched by this edit and must be byte-for-byte identical"
            );
        }
    }
}

#[test]
fn document_xml_changes_only_inside_the_one_edited_runs_text_content() {
    let original_bytes = fs::read(FIXTURE).unwrap();
    let flat = read_docx(&original_bytes).unwrap().body;
    let needle = "Atherton Storage";
    let start = flat.text.find(needle).unwrap();
    let end = start + needle.len();
    let run = *flat
        .run_containing(start, end)
        .expect("the facility name should sit in a single run");

    let edited_bytes = edit_docx(
        &original_bytes,
        &[Edit {
            region: RegionRef::Body,
            flat_start: start,
            flat_end: end,
            replacement: "{{f.name}}".to_string(),
        }],
    )
    .unwrap();

    let orig_xml = read_document_xml(&original_bytes);
    let edited_xml = read_document_xml(&edited_bytes);

    // Everything before the run's own text content, and everything
    // after it, must be byte-for-byte identical -- proving nothing
    // outside the one targeted run moved, not even by a formatting
    // attribute reordering or whitespace change.
    assert_eq!(
        &orig_xml[..run.xml_content_start],
        &edited_xml[..run.xml_content_start],
        "everything before the edited run's text must be untouched"
    );

    let orig_tail = &orig_xml[run.xml_content_end..];
    let edited_tail = &edited_xml[edited_xml.len() - orig_tail.len()..];
    assert_eq!(
        orig_tail, edited_tail,
        "everything after the edited run's text must be untouched"
    );
}

fn read_document_xml(docx_bytes: &[u8]) -> String {
    let mut zip = ZipArchive::new(Cursor::new(docx_bytes)).unwrap();
    let mut entry = zip.by_name("word/document.xml").unwrap();
    let mut contents = String::new();
    entry.read_to_string(&mut contents).unwrap();
    contents
}
