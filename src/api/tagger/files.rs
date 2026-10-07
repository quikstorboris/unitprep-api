//! Reading the uploaded template file from a multipart request.

use axum::extract::Multipart;
use unitprep_core::uploaded_file::UploadedFile;

/// Reads the first file field from `multipart` -- a tagging run is
/// always one `.docx`, not a multi-file upload like UnitGroup's
/// `/upload`. Mirrors `dedup::first_uploaded_file` exactly; kept as its
/// own copy rather than shared, same precedent that function itself
/// already set.
pub(super) async fn first_uploaded_file(
    multipart: &mut Multipart,
) -> Result<Option<UploadedFile>, axum::extract::multipart::MultipartError> {
    let mut result = None;

    while let Some(field) = multipart.next_field().await? {
        let Some(file_name) = field.file_name().map(str::to_string) else {
            continue;
        };
        let relative_path = field.name().unwrap_or(&file_name).to_string();
        let bytes = field.bytes().await?.to_vec();

        if result.is_none() {
            result = Some(UploadedFile {
                file_name,
                relative_path,
                bytes,
                modified_at: None,
            });
        } else {
            tracing::warn!(
                file = %file_name,
                "Ignoring extra multipart field — template tagging takes one file"
            );
        }
    }

    Ok(result)
}
