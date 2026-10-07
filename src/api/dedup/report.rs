//! `POST /dedup/report` -- the report view of a finished check.

use super::dto::DedupSessionRequest;
use crate::api::{dedup_blocking, session_not_found, AppState};
use crate::auth::AuthenticatedUser;
use axum::extract::{Json, State};
use axum::response::{IntoResponse, Response};
use unitprep_core::session_store::SessionStoreExt;

/// Re-fetches a previously computed report — e.g. after a page refresh,
/// without re-uploading the file.
pub async fn report(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<DedupSessionRequest>,
) -> Response {
    match state
        .dedup_sessions
        .with_owned_session(&request.session_id, user.user_id, |session| {
            (session.report.clone(), session.records.clone())
        }) {
        Some((report, records)) => match dedup_blocking::report_view(report, records).await {
            Ok((view, _report, _records)) => Json(view).into_response(),
            Err(response) => response,
        },
        None => session_not_found(&request.session_id),
    }
}
