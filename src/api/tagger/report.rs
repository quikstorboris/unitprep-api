//! `POST /tagger/report` -- the candidates of a finished check.

use super::views::{build_candidate_views, TaggerCheckResponse, TaggerSessionRequest};
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
