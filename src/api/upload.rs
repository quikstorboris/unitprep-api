use std::collections::HashMap;
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

/// The form field name the frontend sends its `File.lastModified` sidecar
/// under — a JSON array of `[file_name, epoch_millis]` pairs, one per
/// uploaded file, keyed by the same name used as each file part's
/// filename (`file.webkitRelativePath || file.name`). Sent as a single
/// extra field rather than folded into each file part, since standard
/// multipart file parts have no metadata slot beyond filename/content-type.
const MODIFIED_TIMES_FIELD: &str = "file_modified_times";

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

    // Populated from the `file_modified_times` sidecar field (see
    // `MODIFIED_TIMES_FIELD`'s doc comment) and applied onto
    // `uploaded_files` once the whole stream has been read — the sidecar
    // isn't guaranteed to arrive before the file parts it describes.
    let mut modified_times: HashMap<String, i64> =
        HashMap::new();

    let mut field_count = 0usize;
    let mut files_failed = 0usize;
    let mut multipart_errors = 0usize;

    loop {
        match multipart.next_field().await {
            Ok(Some(field)) => {
                field_count += 1;

                if field.name()
                    == Some(MODIFIED_TIMES_FIELD)
                {
                    match field.text().await {
                        Ok(text) => {
                            match serde_json::from_str::<
                                Vec<(String, i64)>,
                            >(&text)
                            {
                                Ok(pairs) => {
                                    modified_times
                                        .extend(pairs);
                                }

                                Err(err) => {
                                    tracing::warn!(
                                        field_count,
                                        error = %err,
                                        "Failed parsing file_modified_times sidecar — proceeding without modified-at timestamps"
                                    );
                                }
                            }
                        }

                        Err(err) => {
                            tracing::warn!(
                                field_count,
                                error = %err,
                                "Failed reading file_modified_times sidecar — proceeding without modified-at timestamps"
                            );
                        }
                    }

                    continue;
                }

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
                        // Patched in below once the whole stream (and
                        // thus the sidecar field) has been read.
                        modified_at: None,
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
        modified_times_received = modified_times.len(),
        "Finished multipart processing"
    );

    for uploaded in uploaded_files.iter_mut() {
        if let Some(&ms) =
            modified_times.get(&uploaded.file_name)
        {
            uploaded.modified_at = Some(ms);
        }
    }

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