//! Lets the frontend explicitly free a session it's done with (e.g. the
//! user navigates home/away) instead of always waiting out the 10-minute
//! lazy-expiry timeout. Safe to call on an unknown/already-removed
//! session id — deletion is a no-op in that case.

use std::time::SystemTime;

use axum::{
    extract::{Json, State},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};

use crate::api::AppState;
use unitprep_core::session_store::SessionStoreExt;

#[derive(Debug, Deserialize)]
pub struct CancelSessionRequest {
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct CancelSessionResponse {
    pub success: bool,

    /// Whether a session actually existed to delete. Cancel itself stays
    /// idempotent (always 200, always `success: true`) — deleting an
    /// already-gone session isn't an error worth surfacing as one — but
    /// a caller that does care (debugging a "why didn't this work"
    /// report, say) can still tell the two cases apart instead of both
    /// looking identically successful.
    pub deleted: bool,
}

pub async fn cancel_session(
    State(state): State<AppState>,
    Json(request): Json<CancelSessionRequest>,
) -> impl IntoResponse {
    // Read the session's total lifetime AND mark it cancelled in the same
    // write-locked operation, before the map entry is ever touched. Doing
    // both under one `with_session_mut` call (rather than a separate read
    // then a separate write) means the cancellation flag lands atomically
    // with respect to any concurrent mutator -- whoever acquires this
    // session's lock next, here or in another in-flight request, cannot
    // observe a state where the age was read but cancellation hadn't
    // "started" yet.
    //
    // This is also what closes the concurrent-mutation race `delete`
    // alone could not: a handler racing for this same session's write
    // lock (e.g. `/correct`, via `with_session_mut`) either wins the lock
    // first and completes its mutation before `cancelled` is set here (its
    // write is preserved, same as if cancellation had simply happened a
    // moment later), or loses the race and observes `cancelled == true`
    // once it gets the lock -- at which point `SessionStoreExt`'s default
    // methods (see `core::session_store`) treat the session exactly like
    // a nonexistent one and return `None`, instead of silently applying a
    // mutation to an object about to be detached from the map with no way
    // to ever look it up again.
    let age_ms = state
        .unit_group_sessions
        .with_session_mut(&request.session_id, |session| {
            let age_ms = SystemTime::now()
                .duration_since(session.metadata.created_at)
                .unwrap_or_default()
                .as_millis();

            session.metadata.cancelled = true;

            age_ms
        });

    let deleted = age_ms.is_some();

    state.unit_group_sessions.delete(&request.session_id);

    tracing::info!(
        session_id = %request.session_id,
        age_ms = ?age_ms,
        deleted,
        "Session cancelled"
    );

    Json(CancelSessionResponse {
        success: true,
        deleted,
    })
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::empty_state;
    use crate::application::unit_group_session::Session;

    #[tokio::test]
    async fn cancel_reports_deleted_true_for_a_real_session() {
        let state = empty_state();
        state
            .unit_group_sessions
            .save(Session::new("s1".to_string(), None));

        let response = cancel_session(
            State(state.clone()),
            Json(CancelSessionRequest {
                session_id: "s1".to_string(),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(body["success"], true);
        assert_eq!(body["deleted"], true);
        assert!(state.unit_group_sessions.get_handle("s1").is_none());
    }

    #[tokio::test]
    async fn cancel_stays_idempotent_but_reports_deleted_false_for_unknown_session() {
        let response = cancel_session(
            State(empty_state()),
            Json(CancelSessionRequest {
                session_id: "missing".to_string(),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(body["success"], true);
        assert_eq!(body["deleted"], false);
    }

    /// `session_id` is never parsed as a UUID anywhere in this codebase
    /// -- it's a plain `String` key into the session map -- but a
    /// clearly malformed value (unlike merely an unknown-but-well-formed
    /// one, covered above) is worth its own regression test: a real
    /// client could plausibly send garbage here, and this must return
    /// the same clean "nothing to delete" response, not a panic or a
    /// different error shape.
    #[tokio::test]
    async fn cancel_handles_a_malformed_non_uuid_session_id_cleanly() {
        let response = cancel_session(
            State(empty_state()),
            Json(CancelSessionRequest {
                session_id: "not-a-uuid-!!!-🙃".to_string(),
            }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(body["success"], true);
        assert_eq!(body["deleted"], false);
    }
}
