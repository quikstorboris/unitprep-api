//! The dedup folder scan: classify the files in a folder by their headers,
//! pre-select the right one, and describe what each system needs.
//!
//! Three read-only routes, none of which stores anything:
//!
//! - `POST /dedup/classify-files`: the browser sends each file's name and
//!   header row only (never its contents), so a folder holding card
//!   tokens or SSNs is never uploaded just to find out which file to use.
//! - `POST /dedup/classify-dropbox-folder`: the same answer for a Dropbox
//!   folder. The server has to download each file to read it; nothing is
//!   kept, and only headers are looked at.
//! - `GET /dedup/file-requirements`: the "Files required for
//!   deduplication" panel's data, straight from the registry snapshot.
//!
//! The rules themselves (recognition, alternatives, pre-selection) are
//! `unitprep_dedup::file_selection`; this module is only the HTTP shell.

use std::collections::BTreeMap;

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use std::time::Instant;

use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};

use unitprep_core::parsing::parse_document;
use unitprep_dedup::file_selection::{
    classify, ClassifiedFile, FileFormatMeta, FileHeaders, FileRole, FileStatus, Suggestion,
};

use crate::api::dropbox_browse::{download_as_uploaded_file, ensure_path_in_root};
use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::AuthenticatedUser;

/// Upper bounds on a classify request. A real folder is dozens of files;
/// these only stop a malformed or hostile body from doing unbounded work.
const MAX_FILES: usize = 500;
const MAX_HEADERS_PER_FILE: usize = 2000;

/// How many Dropbox files are fetched at once during a folder scan. Each
/// download is a ~0.4 s network round trip regardless of size, so fetching
/// them one after another made an 8-file folder take ~4 s; a handful in
/// flight at a time keeps it near a single round trip without hammering
/// the API.
const DROPBOX_SCAN_CONCURRENCY: usize = 6;

/// Extensions the ingest can parse -- mirrors the UI's own filter.
fn is_supported_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    [".csv", ".xlsx", ".xls"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

#[derive(Debug, Deserialize)]
pub struct ClassifyFilesRequest {
    pub files: Vec<ClassifyFileInput>,
}

#[derive(Debug, Deserialize)]
pub struct ClassifyFileInput {
    pub file_name: String,
    /// `None` when the browser couldn't read the header row (a legacy
    /// `.xls`); the file is reported as unreadable.
    pub headers: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ClassifyDropboxFolderRequest {
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct ClassifiedFileView {
    pub file_name: String,
    pub path: Option<String>,
    pub status: FileStatus,
    pub format_name: Option<String>,
    pub pms: Option<String>,
    pub report_name: Option<String>,
    pub role: Option<FileRole>,
    pub selection_priority: i32,
}

#[derive(Debug, Serialize)]
pub struct SuggestionView {
    pub pms: Option<String>,
    pub selected: Vec<String>,
    /// file name -> the preferred file it loses to.
    pub alternatives: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct ClassifyResponse {
    pub files: Vec<ClassifiedFileView>,
    pub suggested: SuggestionView,
}

fn view_of(file: ClassifiedFile) -> ClassifiedFileView {
    let (format_name, pms, report_name, role, selection_priority) = match file.format {
        Some(meta) => (
            Some(meta.name),
            Some(meta.pms),
            Some(meta.report_name),
            Some(meta.role),
            meta.selection_priority,
        ),
        None => (None, None, None, None, 0),
    };

    ClassifiedFileView {
        file_name: file.file_name,
        path: file.path,
        status: file.status,
        format_name,
        pms,
        report_name,
        role,
        selection_priority,
    }
}

fn classify_to_response(state: &AppState, files: &[FileHeaders]) -> ClassifyResponse {
    // Synchronous reads of the in-memory registry snapshots -- never a
    // per-request DB call (see `client_ops::vendor_format`).
    let vendors = state.tenant_vendors.read().clone();
    let metas = state.tenant_file_meta.read().clone();

    let (
        classified,
        Suggestion {
            pms,
            selected,
            alternatives,
        },
    ) = classify(files, &vendors, &metas);

    ClassifyResponse {
        files: classified.into_iter().map(view_of).collect(),
        suggested: SuggestionView {
            pms,
            selected,
            alternatives: alternatives.into_iter().collect(),
        },
    }
}

fn bad_request(error: &'static str, message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody { error, message }),
    )
        .into_response()
}

pub async fn classify_files(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
    Json(request): Json<ClassifyFilesRequest>,
) -> Response {
    if request.files.len() > MAX_FILES {
        return bad_request(
            "too_many_files",
            format!("At most {MAX_FILES} files can be classified at once."),
        );
    }
    if request.files.iter().any(|f| {
        f.headers
            .as_ref()
            .is_some_and(|h| h.len() > MAX_HEADERS_PER_FILE)
    }) {
        return bad_request(
            "too_many_columns",
            format!("A file has more than {MAX_HEADERS_PER_FILE} columns."),
        );
    }

    let files: Vec<FileHeaders> = request
        .files
        .into_iter()
        .map(|f| FileHeaders {
            file_name: f.file_name,
            path: None,
            headers: f.headers,
        })
        .collect();

    Json(classify_to_response(&state, &files)).into_response()
}

pub async fn classify_dropbox_folder(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
    Json(request): Json<ClassifyDropboxFolderRequest>,
) -> Response {
    let started = Instant::now();

    if let Err(response) = ensure_path_in_root(&state, &request.path) {
        return response;
    }

    let entries = match state.dropbox.list_folder(&request.path).await {
        Ok(entries) => entries,
        Err(err) => {
            tracing::error!(error = %err, path = %request.path, "Dropbox list_folder failed during dedup folder scan");
            return internal_error("Could not list this Dropbox folder");
        }
    };

    let candidates: Vec<_> = entries
        .into_iter()
        .filter(|entry| !entry.is_folder() && is_supported_name(&entry.name))
        .collect();

    if candidates.is_empty() {
        return bad_request(
            "no_file_uploaded",
            "This Dropbox folder has no CSV or Excel files.".to_string(),
        );
    }
    if candidates.len() > MAX_FILES {
        return bad_request(
            "too_many_files",
            format!("This folder has more than {MAX_FILES} CSV/Excel files."),
        );
    }

    // Each file is downloaded just long enough to read its header row,
    // then dropped. A file that can't be downloaded or parsed is reported
    // as unreadable rather than failing the whole scan. Downloads run a
    // few at a time; `buffered` keeps the results in folder order.
    let state_ref = &state;
    let files: Vec<FileHeaders> = stream::iter(candidates)
        .map(|entry| async move {
            let state = state_ref;
            let headers = match download_as_uploaded_file(state, &entry.path_display).await {
                Ok(file) => match parse_document(&file) {
                    Ok(document) => Some(document.headers),
                    Err(err) => {
                        tracing::warn!(path = %entry.path_display, error = %err, "Dedup folder scan could not parse a file");
                        None
                    }
                },
                Err(_) => {
                    tracing::warn!(path = %entry.path_display, "Dedup folder scan could not download a file");
                    None
                }
            };

            FileHeaders {
                file_name: entry.name.clone(),
                path: Some(entry.path_display.clone()),
                headers,
            }
        })
        .buffered(DROPBOX_SCAN_CONCURRENCY)
        .collect()
        .await;

    crate::api::slow_operation::warn_if_slow("dedup_classify_dropbox_folder", started.elapsed());

    Json(classify_to_response(&state, &files)).into_response()
}

#[derive(Debug, Serialize)]
pub struct RequirementFormat {
    pub name: String,
    pub report_name: String,
    pub role: FileRole,
    pub selection_priority: i32,
    pub guidance: String,
}

#[derive(Debug, Serialize)]
pub struct RequirementVendor {
    pub pms: String,
    pub formats: Vec<RequirementFormat>,
}

#[derive(Debug, Serialize)]
pub struct FileRequirementsResponse {
    pub vendors: Vec<RequirementVendor>,
}

/// Groups the registry's file metadata by PMS, in registry order, with a
/// PMS's usable files (best first) ahead of its supporting ones.
pub fn requirements_from(metas: &[FileFormatMeta]) -> FileRequirementsResponse {
    let mut vendors: Vec<RequirementVendor> = Vec::new();

    for meta in metas {
        let format = RequirementFormat {
            name: meta.name.clone(),
            report_name: meta.report_name.clone(),
            role: meta.role,
            selection_priority: meta.selection_priority,
            guidance: meta.guidance.clone(),
        };
        match vendors.iter_mut().find(|v| v.pms == meta.pms) {
            Some(vendor) => vendor.formats.push(format),
            None => vendors.push(RequirementVendor {
                pms: meta.pms.clone(),
                formats: vec![format],
            }),
        }
    }

    for vendor in &mut vendors {
        vendor.formats.sort_by(|a, b| {
            (a.role == FileRole::Supporting)
                .cmp(&(b.role == FileRole::Supporting))
                .then_with(|| b.selection_priority.cmp(&a.selection_priority))
        });
    }

    FileRequirementsResponse { vendors }
}

pub async fn file_requirements(
    State(state): State<AppState>,
    _user: AuthenticatedUser,
) -> Response {
    let metas = state.tenant_file_meta.read().clone();
    Json(requirements_from(&metas)).into_response()
}

#[cfg(test)]
#[path = "dedup_files_tests.rs"]
mod tests;
