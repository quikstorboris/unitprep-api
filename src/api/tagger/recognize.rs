//! `POST /tagger/check` and the Dropbox import: recognize candidate fields in a template and start a session.

use super::files::first_uploaded_file;
use super::patterns::load_label_proximity_patterns;
use super::views::{
    build_candidate_views, CandidateView, TaggerCheckResponse, TierView, MAX_CANDIDATES,
};
use crate::api::blocking::run_blocking;
use crate::api::dropbox_browse::{download_as_uploaded_file, ensure_path_in_root, parent_folder};
use crate::api::rls::{begin_for, try_response};
use crate::api::{bad_request, error_response, internal_error, AppState};
use crate::application::tagger_session_service::TaggerSessionService;
use crate::auth::AuthenticatedUser;
use crate::client_ops::tool_runs;
use axum::extract::{Json, Multipart, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use docx_surgeon::read_docx;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;
use unitprep_core::uploaded_file::UploadedFile;
use unitprep_tagger_pipeline::find_candidates;

/// Uploads and recognizes a `.docx` in one step, creating a new tagger
/// session. No known values are supplied here -- this is the blank-
/// template case (label-proximity against the pattern library); the
/// filled-document, known-value case (`detect_candidates`) is wired
/// into the same pipeline but has no caller yet, since there's no UI
/// step for an OM to supply known values today.
pub async fn check(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Query(query): Query<TaggerCheckQuery>,
    mut multipart: Multipart,
) -> Response {
    let file = match first_uploaded_file(&mut multipart).await {
        Ok(Some(file)) => file,
        Ok(None) => {
            return bad_request("no_file_uploaded", "No file was uploaded".to_string());
        }
        Err(err) => {
            tracing::error!(error = %err, "Multipart parser error during tagger check");
            return bad_request("multipart_error", err.to_string());
        }
    };

    recognize_and_create_session(&state, &user, file, None, query.facility_id).await
}

/// The facility a tagging run is for, when it was opened from a facility's
/// own Template Tagger page. When present the check is recorded on the
/// facility's Onboarding Work page.
#[derive(Debug, Default, Deserialize)]
pub struct TaggerCheckQuery {
    #[serde(default)]
    pub facility_id: Option<uuid::Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct TaggerDropboxPathRequest {
    pub path: String,
    #[serde(default)]
    pub facility_id: Option<uuid::Uuid>,
}

/// Dropbox-sourced counterpart to `check` -- same recognize/session-create
/// logic via `recognize_and_create_session`, source is a Dropbox path
/// instead of a multipart upload. Mirrors `dedup::import_from_dropbox`.
pub async fn import_from_dropbox(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<TaggerDropboxPathRequest>,
) -> Response {
    if let Err(response) = ensure_path_in_root(&state, &request.path) {
        return response;
    }

    let file = match download_as_uploaded_file(&state, &request.path).await {
        Ok(file) => file,
        Err(response) => return response,
    };

    let source_dropbox_folder_path = parent_folder(&request.path);
    recognize_and_create_session(
        &state,
        &user,
        file,
        source_dropbox_folder_path,
        request.facility_id,
    )
    .await
}

/// The actual "upload+recognize" logic shared by `check` and
/// `import_from_dropbox` -- unlike dedup's much smaller equivalent (a
/// 3-line parse/ingest/report call), this involves a DB round trip for
/// the pattern library plus the `MAX_CANDIDATES` guard, substantial
/// enough that keeping two copies in sync would be a real risk, so this
/// one is shared rather than duplicated per acquisition method (contrast
/// `first_uploaded_file`'s own doc comment, which deliberately chose the
/// opposite tradeoff for a much smaller function).
pub(super) async fn recognize_and_create_session(
    state: &AppState,
    user: &AuthenticatedUser,
    file: UploadedFile,
    source_dropbox_folder_path: Option<String>,
    facility_id: Option<uuid::Uuid>,
) -> Response {
    let started = Instant::now();

    // Parsing the .docx is CPU-bound: off the async workers.
    let bytes_to_read = file.bytes.clone();
    let doc = match run_blocking("tagger_read_docx", move || read_docx(&bytes_to_read)).await {
        Ok(Ok(doc)) => doc,
        Ok(Err(err)) => {
            tracing::warn!(file = %file.file_name, error = ?err, "Tagger check failed to read .docx");
            return bad_request(
                "invalid_docx",
                "Could not read this file as a .docx".to_string(),
            );
        }
        Err(response) => return response,
    };

    let mut tx = try_response!(begin_for(state, user, "Could not load the pattern library").await);

    let patterns = match load_label_proximity_patterns(&mut tx).await {
        Ok(patterns) => patterns,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "tag_pattern lookup query failed");
            return internal_error("Could not load the pattern library");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit tag_pattern lookup transaction");
        return internal_error("Could not load the pattern library");
    }

    // Pattern matching over the whole document and building the views are
    // CPU-bound too; `patterns` is only counted afterwards, so it moves in.
    let pattern_count = patterns.len();
    let found = run_blocking("tagger_find_candidates", move || {
        let candidates = find_candidates(&doc, &[], &patterns);
        if candidates.len() > MAX_CANDIDATES {
            return Err(candidates.len());
        }
        let views = build_candidate_views(&doc, &candidates);
        Ok((candidates, views))
    })
    .await;
    let (candidates, candidate_views) = match found {
        Ok(Ok(found)) => found,
        Ok(Err(candidate_count)) => {
            tracing::warn!(
                file = %file.file_name,
                candidate_count,
                "Tagger check rejected -- candidate count exceeds MAX_CANDIDATES"
            );
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "too_many_candidates",
                format!(
                    "This document has too many potential matches to review ({candidate_count} found, {MAX_CANDIDATES} max). \
                     It may not be a template intended for tagging."
                ),
            );
        }
        Err(response) => return response,
    };

    let summary = check_summary(&file.file_name, &candidate_views);

    // Creating the session persists the file, so it runs off the workers as well.
    let sessions = Arc::clone(&state.tagger_sessions);
    let session_bytes = file.bytes;
    let session_file_name = file.file_name.clone();
    let owner_id = user.user_id;
    let session_id = match run_blocking("tagger_create_session", move || {
        TaggerSessionService::new(sessions).create_session(
            session_bytes,
            session_file_name,
            candidates,
            Some(owner_id),
            source_dropbox_folder_path,
        )
    })
    .await
    {
        Ok(session_id) => session_id,
        Err(response) => return response,
    };

    if let Some(facility_id) = facility_id {
        tool_runs::record_run(
            &state.db,
            tool_runs::RunRecord {
                tool: "tagger",
                facility_id,
                session_id: &session_id,
                actor_user_id: user.user_id,
                role_keys: &user.role_keys,
                source_file_name: &file.file_name,
                report_summary: summary,
            },
        )
        .await;
    }

    tracing::info!(
        session_id = %session_id,
        owner_id = %user.user_id,
        file = %file.file_name,
        pattern_count,
        candidate_count = candidate_views.len(),
        check_ms = started.elapsed().as_millis(),
        "Tagger check complete"
    );
    crate::api::slow_operation::warn_if_slow("tagger_check", started.elapsed());

    Json(TaggerCheckResponse {
        session_id,
        candidates: candidate_views,
    })
    .into_response()
}

/// The stored summary for a tagging run's check: the template, how many
/// places were found and how sure the matcher was about them. The reviewer's
/// result (how many were applied) is merged in later by `apply`.
pub(super) fn check_summary(file_name: &str, candidates: &[CandidateView]) -> serde_json::Value {
    let mut by_tag: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for candidate in candidates {
        *by_tag.entry(candidate.tag_key.as_str()).or_default() += 1;
    }
    let needs_review = candidates
        .iter()
        .filter(|c| matches!(c.tier, TierView::NeedsReview))
        .count();

    serde_json::json!({
        "template_file": file_name,
        "candidate_count": candidates.len(),
        "needs_review_count": needs_review,
        "tags": by_tag,
        "applied_count": null,
    })
}
