//! Shared multipart handling for the two manual-file-upload endpoints
//! (`unit_file_upload`, `group_file_upload`) -- both accept the exact
//! same two-field shape (`session_id` text field, `file` file field) and
//! only differ in which discovery field gets forced afterward.

use axum::extract::multipart::MultipartError;
use axum::extract::Multipart;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use unitprep_core::uploaded_file::UploadedFile;

use crate::api::ApiErrorBody;

pub(crate) struct ManualUploadFields {
    pub session_id: String,
    pub file: UploadedFile,
}

pub(crate) enum ManualUploadError {
    Multipart(MultipartError),
    MissingSessionId,
    MissingFile,
    ParseFailed(String),
}

pub(crate) async fn extract_manual_upload_fields(
    mut multipart: Multipart,
) -> Result<ManualUploadFields, ManualUploadError> {
    let mut session_id: Option<String> = None;
    let mut file_name: Option<String> = None;
    let mut bytes: Option<Vec<u8>> = None;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(err) => return Err(ManualUploadError::Multipart(err)),
        };

        if field.name() == Some("session_id") {
            if let Ok(text) = field.text().await {
                session_id = Some(text);
            }

            continue;
        }

        if field.name() == Some("file") {
            file_name = field.file_name().map(|name| name.to_string());

            bytes = Some(
                field
                    .bytes()
                    .await
                    .map_err(ManualUploadError::Multipart)?
                    .to_vec(),
            );
        }
    }

    let session_id = session_id.ok_or(ManualUploadError::MissingSessionId)?;
    let file_name = file_name.ok_or(ManualUploadError::MissingFile)?;
    let bytes = bytes.ok_or(ManualUploadError::MissingFile)?;

    Ok(ManualUploadFields {
        session_id,
        file: UploadedFile {
            file_name: file_name.clone(),
            relative_path: file_name,
            bytes,
            modified_at: None,
        },
    })
}

pub(crate) fn manual_upload_error_response(err: ManualUploadError) -> Response {
    match err {
        ManualUploadError::Multipart(err) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "multipart_error",
                message: format!("Failed reading the uploaded file: {err}"),
            }),
        )
            .into_response(),

        ManualUploadError::MissingSessionId => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "missing_session_id",
                message: "Request is missing the session_id field.".to_string(),
            }),
        )
            .into_response(),

        ManualUploadError::MissingFile => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "missing_file",
                message: "Request is missing the file field.".to_string(),
            }),
        )
            .into_response(),

        ManualUploadError::ParseFailed(context) => (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "file_parse_failed",
                message: context,
            }),
        )
            .into_response(),
    }
}
