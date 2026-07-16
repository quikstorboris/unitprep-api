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
    // Read the session's total lifetime before it's gone — distinct from
    // (and not derivable from) last_accessed, which only tells you how
    // long it sat idle. Both matter for different diagnostic questions,
    // which is why this is worth reading even though we're about to
    // delete the session anyway.
    let age_ms = state
        .unit_group_sessions
        .with_session(
            &request.session_id,
            |session| {
                SystemTime::now()
                    .duration_since(
                        session
                            .metadata
                            .created_at,
                    )
                    .unwrap_or_default()
                    .as_millis()
            },
        );

    let deleted = age_ms.is_some();

    state
        .unit_group_sessions
        .delete(&request.session_id);

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
        state.unit_group_sessions.save(Session::new("s1".to_string()));

        let response = cancel_session(
            State(state.clone()),
            Json(CancelSessionRequest { session_id: "s1".to_string() }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(body["success"], true);
        assert_eq!(body["deleted"], true);
        assert!(state.unit_group_sessions.get_handle("s1").is_none());
    }

    #[tokio::test]
    async fn cancel_stays_idempotent_but_reports_deleted_false_for_unknown_session() {
        let response = cancel_session(
            State(empty_state()),
            Json(CancelSessionRequest { session_id: "missing".to_string() }),
        )
        .await
        .into_response();

        assert_eq!(response.status(), StatusCode::OK);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        assert_eq!(body["success"], true);
        assert_eq!(body["deleted"], false);
    }
}
