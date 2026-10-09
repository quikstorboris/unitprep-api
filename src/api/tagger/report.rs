//! `POST /tagger/report` -- the candidates of a finished check.

use super::views::{build_candidate_views, TaggerCheckResponse, TaggerSessionRequest};
use crate::api::blocking::run_blocking;
use crate::api::{internal_error, session_not_found, AppState};
use crate::auth::AuthenticatedUser;
use axum::extract::{Json, State};
use axum::response::{IntoResponse, Response};
use docx_surgeon::read_docx;
use unitprep_core::session_store::SessionStoreExt;

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
        None => return session_not_found(&request.session_id),
    };

    // read_docx already validated these exact bytes at /check time, so
    // a failure here can only mean something is very wrong with the
    // stored session bytes, not with the file itself.
    // Re-parsing the document and rebuilding the views is CPU-bound.
    let views = run_blocking("tagger_report", move || {
        read_docx(&original_bytes).map(|doc| build_candidate_views(&doc, &candidates))
    })
    .await;
    let candidates = match views {
        Ok(Ok(candidates)) => candidates,
        Ok(Err(err)) => {
            tracing::error!(session_id = %request.session_id, error = ?err, "Tagger report failed to re-read the stored document");
            return internal_error("Could not rebuild this session's document");
        }
        Err(response) => return response,
    };

    Json(TaggerCheckResponse {
        session_id: request.session_id,
        candidates,
    })
    .into_response()
}
