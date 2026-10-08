//! `POST /tagger/apply` -- writes the confirmed substitutions into a copy of the template.

use super::dropbox::tagged_file_name;
use crate::api::session_io::attachment_response;
use crate::api::{bad_request, internal_error, session_not_found, AppState};
use crate::auth::AuthenticatedUser;
use crate::client_ops::tool_runs;
use axum::extract::{Json, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use docx_surgeon::{edit_docx_all, read_docx, Edit, UnderlineEdit};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use unitprep_core::session_store::SessionStoreExt;
use unitprep_tagger_pipeline::{to_edit, AppliedEdit, SubstitutionStyle};

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
    let (edited_bytes, file_name) = match build_edited_docx(&state, &user, &request).await {
        Ok(built) => built,
        Err(response) => return response,
    };

    record_applied(
        &state,
        &user,
        &request.session_id,
        request.confirmed.len(),
        request.preserve_blanks,
    )
    .await;
    tool_runs::attach_output_bytes(
        &state.db,
        user.user_id,
        &user.role_keys,
        &request.session_id,
        edited_bytes.clone(),
        DOCX_CONTENT_TYPE,
        &file_name,
    )
    .await;

    attachment_response(edited_bytes, DOCX_CONTENT_TYPE, &file_name)
}

pub(super) const DOCX_CONTENT_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// Adds the reviewer's result to the run's stored summary (a no-op when
/// the run was not recorded).
pub(super) async fn record_applied(
    state: &AppState,
    user: &AuthenticatedUser,
    session_id: &str,
    applied_count: usize,
    preserve_blanks: bool,
) {
    tool_runs::merge_report_summary(
        &state.db,
        user.user_id,
        &user.role_keys,
        session_id,
        &serde_json::json!({
            "applied_count": applied_count,
            "preserve_blanks": preserve_blanks,
        }),
    )
    .await;
}

/// The shared "re-read the stored document, build and validate every
/// confirmed edit, splice them in" logic behind both `apply` (returns
/// the result as a download) and `apply_to_dropbox` (uploads it
/// instead) -- substantial enough (a whole per-candidate validation
/// loop with its own structured failure reporting) that, like
/// `recognize_and_create_session` above, it's shared rather than kept
/// as two copies.
#[allow(clippy::result_large_err)]
pub(super) async fn build_edited_docx(
    state: &AppState,
    user: &AuthenticatedUser,
    request: &TaggerApplyRequest,
) -> Result<(Vec<u8>, String), Response> {
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
        None => return Err(session_not_found(&request.session_id)),
    };

    // read_docx already validated these exact bytes at /check time, so
    // a failure here can only mean something is very wrong with the
    // stored session bytes, not with the file itself.
    let doc = match read_docx(&original_bytes) {
        Ok(doc) => doc,
        Err(err) => {
            tracing::error!(session_id = %request.session_id, error = ?err, "Tagger apply failed to re-read the stored document");
            return Err(internal_error("Could not rebuild this session's document"));
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
            return Err(bad_request(
                "invalid_candidate_index",
                format!(
                    "No candidate at index {} in this session",
                    confirmed.candidate_index
                ),
            ));
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
        return Err((
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
            .into_response());
    }

    let edited_bytes = match edit_docx_all(&original_bytes, &edits, &underline_edits) {
        Ok(bytes) => bytes,
        Err(err) => {
            tracing::warn!(session_id = %request.session_id, error = ?err, "Tagger apply failed");
            return Err(bad_request(
                "apply_failed",
                "Could not apply the confirmed substitutions".to_string(),
            ));
        }
    };

    tracing::info!(
        session_id = %request.session_id,
        owner_id = %user.user_id,
        confirmed_count = request.confirmed.len(),
        apply_ms = started.elapsed().as_millis(),
        "Tagger apply complete"
    );

    Ok((edited_bytes, tagged_file_name(&original_file_name)))
}
