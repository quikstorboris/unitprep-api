use crate::parsing::parse_document;
use crate::uploaded_file::UploadedFile;

#[test]
fn unsupported_file_fails() {
    let file = UploadedFile {
        file_name: "test.json".to_string(),
        relative_path: String::new(),
        bytes: b"{}".to_vec(),
    };

    assert!(
        parse_document(&file).is_err()
    );
}
