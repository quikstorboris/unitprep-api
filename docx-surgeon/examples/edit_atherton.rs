use docx_surgeon::{edit_docx, read_docx, Edit};
use std::fs;

fn main() {
    let original_bytes = fs::read("tests/fixtures/atherton-storage-contract.docx").unwrap();
    let flat = read_docx(&original_bytes).unwrap().body;
    let needle = "Atherton Storage";
    let start = flat.text.find(needle).unwrap();
    let end = start + needle.len();

    let edited_bytes = edit_docx(
        &original_bytes,
        &[Edit {
            flat_start: start,
            flat_end: end,
            replacement: "{{f.name}}".to_string(),
        }],
    )
    .unwrap();

    fs::write("/tmp/atherton-edited.docx", &edited_bytes).unwrap();
    println!("wrote {} bytes", edited_bytes.len());
}
