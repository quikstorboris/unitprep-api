//! HTTP layer for the QMS Template Tagging Assistant. Session-based like
//! dedup: upload+recognize happens in one step (`check`), there's no
//! separate "analyze" stage to wait for. Unlike dedup, `check` needs a DB
//! round trip first (the active `client_ops.tag_pattern` label-proximity
//! library) -- that lookup lives here, not in `TaggerSessionService`,
//! since the service has no business owning a DB connection.

use std::sync::Arc;
use std::time::Instant;

use axum::{
    extract::{Json, Multipart, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use docx_surgeon::{edit_docx_all, read_docx, Edit, RegionRef, UnderlineEdit};
use unitprep_core::session_store::SessionStoreExt;
use unitprep_core::uploaded_file::UploadedFile;
use unitprep_tagger_pipeline::{
    find_candidates, to_edit, AppliedEdit, ConfidenceTier, RegionCandidate, SubstitutionStyle,
};
use unitprep_template_tagger::{LabelPosition, LabelProximityPattern};

use crate::api::{internal_error, session_not_found, ApiErrorBody, AppState};
use crate::application::tagger_session_service::TaggerSessionService;
use crate::auth::{begin_rls_transaction, AuthenticatedUser};

/// How much surrounding text a candidate's snippet carries on each side
/// -- enough to read the label/context around a match without sending
/// the whole region back to the browser.
const SNIPPET_CONTEXT_CHARS: usize = 30;

/// Hard ceiling on how many candidates one `/tagger/check` run will
/// process past `find_candidates` -- a real template's candidate count is
/// "tens, not thousands" per `assign_tiers`'s own doc comment; this bounds
/// a pathological or adversarial document (e.g. a blank repeated
/// thousands of times) well above any real template but far below where
/// building candidate views, cloning matched text, and storing the
/// session would become its own resource concern.
const MAX_CANDIDATES: usize = 2000;

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionView {
    Body,
    TableCell { index: usize },
}

impl From<RegionRef> for RegionView {
    fn from(region: RegionRef) -> Self {
        match region {
            RegionRef::Body => RegionView::Body,
            RegionRef::TableCell(index) => RegionView::TableCell { index },
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TierView {
    Auto,
    NeedsReview,
}

impl From<ConfidenceTier> for TierView {
    fn from(tier: ConfidenceTier) -> Self {
        match tier {
            ConfidenceTier::Auto => TierView::Auto,
            ConfidenceTier::NeedsReview => TierView::NeedsReview,
        }
    }
}

/// One candidate as the review UI sees it. `index` is this candidate's
/// position in the session's own candidate list -- `/tagger/apply`
/// references a candidate by this same index, not by re-sending its
/// coordinates.
#[derive(Debug, Serialize)]
pub struct CandidateView {
    pub index: usize,
    pub region: RegionView,
    pub tag_key: String,
    pub matched_text: String,
    pub tier: TierView,
    pub snippet: String,
}

fn build_candidate_views(
    doc: &docx_surgeon::FlatDocument,
    candidates: &[RegionCandidate],
) -> Vec<CandidateView> {
    candidates
        .iter()
        .enumerate()
        .map(|(index, rc)| {
            let region_text = match rc.region {
                RegionRef::Body => &doc.body.text,
                RegionRef::TableCell(i) => &doc.table_cells[i].text,
            };
            CandidateView {
                index,
                region: rc.region.into(),
                tag_key: rc.candidate.tag_key.clone(),
                matched_text: rc.candidate.matched_text.clone(),
                tier: rc.tier.into(),
                snippet: build_snippet(region_text, rc.candidate.start, rc.candidate.end),
            }
        })
        .collect()
}

fn char_boundary_at_or_after(text: &str, mut idx: usize) -> usize {
    while idx < text.len() && !text.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

fn char_boundary_at_or_before(text: &str, mut idx: usize) -> usize {
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn build_snippet(text: &str, start: usize, end: usize) -> String {
    let snippet_start =
        char_boundary_at_or_after(text, start.saturating_sub(SNIPPET_CONTEXT_CHARS));
    let snippet_end =
        char_boundary_at_or_before(text, (end + SNIPPET_CONTEXT_CHARS).min(text.len()));

    let mut snippet = String::new();
    if snippet_start > 0 {
        snippet.push('\u{2026}');
    }
    snippet.push_str(&text[snippet_start..snippet_end]);
    if snippet_end < text.len() {
        snippet.push('\u{2026}');
    }
    snippet
}

#[derive(Debug, Serialize)]
pub struct TaggerCheckResponse {
    pub session_id: String,
    pub candidates: Vec<CandidateView>,
}

#[derive(Debug, Deserialize)]
pub struct TaggerSessionRequest {
    pub session_id: String,
}

/// Reads the first file field from `multipart` -- a tagging run is
/// always one `.docx`, not a multi-file upload like UnitGroup's
/// `/upload`. Mirrors `dedup::first_uploaded_file` exactly; kept as its
/// own copy rather than shared, same precedent that function itself
/// already set.
async fn first_uploaded_file(
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

#[derive(Debug, Deserialize)]
struct LabelProximityPatternJson {
    label: String,
    position: String,
    max_gap_chars: usize,
    #[serde(default)]
    requires_preceding_anchor: Option<PrecedingAnchorJson>,
}

#[derive(Debug, Deserialize)]
struct PrecedingAnchorJson {
    text: String,
    within_chars: usize,
}

#[derive(Debug, sqlx::FromRow)]
struct PatternRow {
    tag_key: String,
    pattern: serde_json::Value,
}

/// Loads every active `label_proximity` pattern from `client_ops.tag_pattern`.
/// A row whose `pattern` JSONB doesn't parse into the expected shape, or
/// names an unrecognized `position`, is logged and skipped rather than
/// failing the whole request -- one malformed pattern (most likely from
/// hand-authored data, since there's no admin UI for this table yet)
/// should not block every other tag from being recognized.
async fn load_label_proximity_patterns(
    tx: &mut sqlx::PgConnection,
) -> Result<Vec<LabelProximityPattern>, sqlx::Error> {
    let rows: Vec<PatternRow> = sqlx::query_as(
        "SELECT tag_key, pattern FROM client_ops.tag_pattern
          WHERE kind = 'label_proximity' AND is_active = true",
    )
    .fetch_all(tx)
    .await?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let parsed: LabelProximityPatternJson = match serde_json::from_value(row.pattern) {
                Ok(parsed) => parsed,
                Err(err) => {
                    tracing::warn!(tag_key = %row.tag_key, error = %err, "Skipping malformed tag_pattern row");
                    return None;
                }
            };
            let position = match parsed.position.as_str() {
                "before" => LabelPosition::Before,
                "after" => LabelPosition::After,
                other => {
                    tracing::warn!(tag_key = %row.tag_key, position = %other, "Skipping tag_pattern row with unrecognized position");
                    return None;
                }
            };
            Some(LabelProximityPattern {
                tag_key: row.tag_key,
                label: parsed.label,
                position,
                max_gap_chars: parsed.max_gap_chars,
                requires_preceding_anchor: parsed.requires_preceding_anchor.map(|a| {
                    unitprep_template_tagger::PrecedingAnchor {
                        text: a.text,
                        within_chars: a.within_chars,
                    }
                }),
            })
        })
        .collect())
}

/// Uploads and recognizes a `.docx` in one step, creating a new tagger
/// session. No known values are supplied here -- this is the blank-
/// template case (label-proximity against the pattern library); the
/// filled-document, known-value case (`detect_candidates`) is wired
/// into the same pipeline but has no caller yet, since there's no UI
/// step for an OM to supply known values today.
pub async fn check(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    mut multipart: Multipart,
) -> Response {
    let started = Instant::now();

    let file = match first_uploaded_file(&mut multipart).await {
        Ok(Some(file)) => file,
        Ok(None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiErrorBody {
                    error: "no_file_uploaded",
                    message: "No file was uploaded".to_string(),
                }),
            )
                .into_response();
        }
        Err(err) => {
            tracing::error!(error = %err, "Multipart parser error during tagger check");
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiErrorBody {
                    error: "multipart_error",
                    message: err.to_string(),
                }),
            )
                .into_response();
        }
    };

    let doc = match read_docx(&file.bytes) {
        Ok(doc) => doc,
        Err(err) => {
            tracing::warn!(file = %file.file_name, error = ?err, "Tagger check failed to read .docx");
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiErrorBody {
                    error: "invalid_docx",
                    message: "Could not read this file as a .docx".to_string(),
                }),
            )
                .into_response();
        }
    };

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for tag_pattern lookup");
            return internal_error("Could not load the pattern library");
        }
    };

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

    let candidates = find_candidates(&doc, &[], &patterns);

    if candidates.len() > MAX_CANDIDATES {
        tracing::warn!(
            file = %file.file_name,
            candidate_count = candidates.len(),
            "Tagger check rejected -- candidate count exceeds MAX_CANDIDATES"
        );
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ApiErrorBody {
                error: "too_many_candidates",
                message: format!(
                    "This document has too many potential matches to review ({} found, {} max). \
                     It may not be a template intended for tagging.",
                    candidates.len(),
                    MAX_CANDIDATES
                ),
            }),
        )
            .into_response();
    }

    let candidate_views = build_candidate_views(&doc, &candidates);

    let session_id = TaggerSessionService::new(Arc::clone(&state.tagger_sessions)).create_session(
        file.bytes,
        file.file_name.clone(),
        candidates,
        Some(user.user_id),
    );

    tracing::info!(
        session_id = %session_id,
        owner_id = %user.user_id,
        file = %file.file_name,
        pattern_count = patterns.len(),
        candidate_count = candidate_views.len(),
        check_ms = started.elapsed().as_millis(),
        "Tagger check complete"
    );

    Json(TaggerCheckResponse {
        session_id,
        candidates: candidate_views,
    })
    .into_response()
}

/// Re-fetches a previously computed candidate list -- e.g. after a page
/// refresh, without re-uploading the file.
pub async fn report(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<TaggerSessionRequest>,
) -> Response {
    let session_data =
        state
            .tagger_sessions
            .with_owned_session(&request.session_id, user.user_id, |session| {
                (session.original_bytes.clone(), session.candidates.clone())
            });

    let (original_bytes, candidates) = match session_data {
        Some(data) => data,
        None => return session_not_found(),
    };

    // read_docx already validated these exact bytes at /check time, so
    // a failure here can only mean something is very wrong with the
    // stored session bytes, not with the file itself.
    let doc = match read_docx(&original_bytes) {
        Ok(doc) => doc,
        Err(err) => {
            tracing::error!(session_id = %request.session_id, error = ?err, "Tagger report failed to re-read the stored document");
            return internal_error("Could not rebuild this session's document");
        }
    };

    Json(TaggerCheckResponse {
        session_id: request.session_id,
        candidates: build_candidate_views(&doc, &candidates),
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct ConfirmedSubstitution {
    /// Index into the session's own candidate list, as returned by
    /// `/tagger/check` or `/tagger/report`.
    pub candidate_index: usize,
    /// The tag to actually apply -- lets a reviewer override an
    /// ambiguous (`NeedsReview`) candidate's default guess rather than
    /// being stuck with whichever pattern happened to match first.
    pub tag_key: String,
}

#[derive(Debug, Deserialize)]
pub struct TaggerApplyRequest {
    pub session_id: String,
    pub confirmed: Vec<ConfirmedSubstitution>,
    /// When true, every confirmed substitution keeps its matched span
    /// (the blank, or, for a `detect_candidates` match, the already-
    /// filled value) instead of replacing it outright -- the tag lands
    /// centered inside it, with whatever's left of the original text
    /// split evenly on either side (see
    /// `SubstitutionStyle::PreserveBlank`). An OM-facing style choice,
    /// applied uniformly to the whole apply call, not something this
    /// handler has an opinion on. Defaults to `false` (replace outright,
    /// the original behavior) so an older caller that never sends this
    /// field keeps working unchanged.
    #[serde(default)]
    pub preserve_blanks: bool,
}

/// One confirmed substitution that couldn't be turned into an edit --
/// reported back so the reviewer knows exactly which one to uncheck,
/// rather than an opaque all-or-nothing failure.
#[derive(Debug, Serialize)]
pub struct FailedSubstitution {
    pub candidate_index: usize,
    pub tag_key: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct TaggerApplyErrorBody {
    pub error: &'static str,
    pub message: String,
    pub failed: Vec<FailedSubstitution>,
}

/// Applies every confirmed substitution and returns the finished
/// `.docx` for download. Nothing is applied that wasn't named in
/// `confirmed` -- the "propose, never modify" rule both matchers and
/// docx-surgeon already hold is enforced structurally here too: this
/// handler only ever builds edits from candidates the caller explicitly
/// listed, never from `session.candidates` wholesale.
///
/// Every edit is checked against the document *before* any are
/// applied, even though docx-surgeon can now splice an edit across
/// several runs (a blank's underscore run is often split across
/// multiple `<w:t>` elements in the real XML -- a formatting change, a
/// spell-check restart point, anything that gives Word a reason to end
/// one run and start another mid-span, even though it reads as one
/// unbroken blank on screen). The remaining failure mode this still
/// catches is coordinates that touch no run at all (a stale session).
/// The alternative -- letting edit_docx fail the whole batch on the
/// first bad one -- would still give the reviewer no way to tell which
/// confirmation was the problem, only "something failed."
pub async fn apply(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<TaggerApplyRequest>,
) -> Response {
    let started = Instant::now();

    let session_data =
        state
            .tagger_sessions
            .with_owned_session(&request.session_id, user.user_id, |session| {
                (
                    session.original_bytes.clone(),
                    session.original_file_name.clone(),
                    session.candidates.clone(),
                )
            });

    let (original_bytes, original_file_name, candidates) = match session_data {
        Some(data) => data,
        None => return session_not_found(),
    };

    // read_docx already validated these exact bytes at /check time, so
    // a failure here can only mean something is very wrong with the
    // stored session bytes, not with the file itself.
    let doc = match read_docx(&original_bytes) {
        Ok(doc) => doc,
        Err(err) => {
            tracing::error!(session_id = %request.session_id, error = ?err, "Tagger apply failed to re-read the stored document");
            return internal_error("Could not rebuild this session's document");
        }
    };

    let style = if request.preserve_blanks {
        SubstitutionStyle::PreserveBlank
    } else {
        SubstitutionStyle::Replace
    };

    let mut edits: Vec<Edit> = Vec::new();
    let mut underline_edits: Vec<UnderlineEdit> = Vec::new();
    let mut failed = Vec::new();
    for confirmed in &request.confirmed {
        let Some(candidate) = candidates.get(confirmed.candidate_index) else {
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiErrorBody {
                    error: "invalid_candidate_index",
                    message: format!(
                        "No candidate at index {} in this session",
                        confirmed.candidate_index
                    ),
                }),
            )
                .into_response();
        };

        let applied = to_edit(candidate, format!("{{{{{}}}}}", confirmed.tag_key), style);
        let (region, editable) = match &applied {
            AppliedEdit::Plain(edit) => (edit.region, (edit.flat_start, edit.flat_end)),
            AppliedEdit::Underline(edit) => (edit.region, (edit.flat_start, edit.flat_end)),
        };
        let region_text = doc.region(region);
        if !region_text.is_editable_range(editable.0, editable.1) {
            failed.push(FailedSubstitution {
                candidate_index: confirmed.candidate_index,
                tag_key: confirmed.tag_key.clone(),
                reason: "This text doesn't correspond to any position in the document \
                         (the session may be stale) -- try re-uploading."
                    .to_string(),
            });
            continue;
        }
        match applied {
            AppliedEdit::Plain(edit) => edits.push(edit),
            AppliedEdit::Underline(edit) => underline_edits.push(edit),
        }
    }

    if !failed.is_empty() {
        tracing::warn!(
            session_id = %request.session_id,
            failed_count = failed.len(),
            "Tagger apply rejected -- one or more confirmed substitutions cannot be applied"
        );
        // Names the specific tag(s) directly in `message` -- not just in
        // `failed` -- so the existing generic error-banner display (which
        // only ever shows `message`) is still actionable without needing
        // a dedicated per-row UI treatment.
        let tag_list = failed
            .iter()
            .map(|f| f.tag_key.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return (
            StatusCode::BAD_REQUEST,
            Json(TaggerApplyErrorBody {
                error: "unappliable_substitutions",
                message: format!(
                    "Could not apply: {tag_list} -- the matched text spans more than one \
                     formatting run in the document. Uncheck {} and try again.",
                    if failed.len() == 1 { "it" } else { "these" }
                ),
                failed,
            }),
        )
            .into_response();
    }

    let edited_bytes = match edit_docx_all(&original_bytes, &edits, &underline_edits) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!(session_id = %request.session_id, error = ?err, "Tagger apply failed");
            return (
                StatusCode::BAD_REQUEST,
                Json(ApiErrorBody {
                    error: "apply_failed",
                    message: "Could not apply the confirmed substitutions".to_string(),
                }),
            )
                .into_response();
        }
    };

    tracing::info!(
        session_id = %request.session_id,
        owner_id = %user.user_id,
        confirmed_count = request.confirmed.len(),
        apply_ms = started.elapsed().as_millis(),
        "Tagger apply complete"
    );

    file_response(edited_bytes, &tagged_file_name(&original_file_name))
}

fn tagged_file_name(original: &str) -> String {
    match original.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}-tagged.{ext}"),
        None => format!("{original}-tagged"),
    }
}

fn file_response(bytes: Vec<u8>, file_name: &str) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            .parse()
            .unwrap(),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        format!("attachment; filename=\"{file_name}\"")
            .parse()
            .unwrap(),
    );
    (headers, bytes).into_response()
}

#[cfg(test)]
#[path = "tagger_tests.rs"]
mod tests;
