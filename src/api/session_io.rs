//! Pieces every tool session (Group Prep, Duplicate Check, Template Tagger)
//! shares on the way out: the download response for a finished file, and
//! the "where should Save to Dropbox default to" answer.

use axum::{
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// The two headers every file download carries: its content type and a
/// `Content-Disposition: attachment` naming the file. Split from
/// [`attachment_response`] for the handlers that add their own headers or
/// build the body separately (the CSV and PDF exports).
pub(crate) fn attachment_headers(content_type: &str, file_name: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, content_type.parse().unwrap());
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{file_name}\"")
            .parse()
            .unwrap(),
    );
    headers
}

/// A finished file as a download.
pub(crate) fn attachment_response(bytes: Vec<u8>, content_type: &str, file_name: &str) -> Response {
    (attachment_headers(content_type, file_name), bytes).into_response()
}

/// The answer to "where should the save-to-Dropbox picker open", the same
/// shape for every tool.
#[derive(Debug, Serialize)]
pub struct SaveLocationResponse {
    /// `Some(path)` when this session's source file was imported from
    /// Dropbox -- the tool's own output subfolder next to wherever that
    /// file actually came from, which the frontend's save-to-Dropbox
    /// picker should default `initialPath` to. `None` for a
    /// locally-uploaded session, which has no Dropbox origin to anchor a
    /// default to; the picker falls back to its own existing behavior.
    pub default_folder_path: Option<String>,
}

impl SaveLocationResponse {
    /// `output_folder_name` inside the session's source Dropbox folder.
    /// Computed, not created -- the folder is made at the moment something
    /// is actually saved into it.
    pub fn next_to(source_folder: Option<String>, output_folder_name: &str) -> Self {
        Self {
            default_folder_path: source_folder
                .map(|folder| format!("{folder}/{output_folder_name}")),
        }
    }
}
