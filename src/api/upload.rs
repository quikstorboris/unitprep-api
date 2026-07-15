use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Multipart, State},
    response::IntoResponse,
    Json,
};
use serde::Serialize;

use crate::api::AppState;
use crate::application::session_service::SessionService;
use unitprep_core::uploaded_file::UploadedFile;

#[derive(Serialize)]
pub struct UploadResponse {
    pub session_id: String,
    pub files_uploaded: usize,
    pub files_failed: usize,
    pub multipart_errors: usize,
}

pub async fn upload(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let started = Instant::now();

    let mut uploaded_files: Vec<UploadedFile> =
        Vec::new();

    let mut field_count = 0usize;
    let mut files_failed = 0usize;
    let mut multipart_errors = 0usize;

    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                field_count += 1;

                let file_name =
                    match field.file_name() {
                        Some(name) => {
                            name.to_string()
                        }
                        None => {
                            files_failed += 1;

                            tracing::warn!(
                                field_count,
                                failed_count = files_failed,
                                "Multipart field missing filename"
                            );

                            continue;
                        }
                    };

                let relative_path = field
                    .name()
                    .unwrap_or(&file_name)
                    .to_string();

                let bytes =
                    match field.bytes().await {
                        Ok(bytes) => {
                            bytes.to_vec()
                        }

                        Err(err) => {
                            files_failed += 1;

                            tracing::error!(
                                field_count,
                                file = %file_name,
                                failed_count = files_failed,
                                error = %err,
                                "Failed reading file bytes"
                            );

                            continue;
                        }
                    };

                uploaded_files.push(
                    UploadedFile {
                        file_name,
                        relative_path,
                        bytes,
                    },
                );
            }

            Ok(None) => {
                tracing::info!(
                    field_count,
                    uploaded_count = uploaded_files.len(),
                    failed_count = files_failed,
                    multipart_errors,
                    "Reached end of multipart stream"
                );

                break;
            }

            Err(err) => {
                multipart_errors += 1;

                tracing::error!(
                    field_count,
                    multipart_errors,
                    uploaded_count = uploaded_files.len(),
                    failed_count = files_failed,
                    error = %err,
                    "Multipart parser error encountered"
                );

                break;
            }
        }
    }

    tracing::info!(
        field_count,
        uploaded_count = uploaded_files.len(),
        failed_count = files_failed,
        multipart_errors,
        "Finished multipart processing"
    );

    let files_uploaded =
        uploaded_files.len();

    let session_id =
        SessionService::new(
            Arc::clone(
                &state.unit_group_sessions,
            ),
        )
        .create_session(
            uploaded_files,
        );

    tracing::info!(
        session_id = %session_id,
        files_uploaded,
        files_failed,
        multipart_errors,
        upload_ms =
            started
                .elapsed()
                .as_millis(),
        "Upload complete"
    );

    Json(UploadResponse {
        session_id,
        files_uploaded,
        files_failed,
        multipart_errors,
    })
}