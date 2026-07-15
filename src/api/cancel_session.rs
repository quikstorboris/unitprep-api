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
        .session_store
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

    state
        .session_store
        .delete(&request.session_id);

    tracing::info!(
        session_id = %request.session_id,
        age_ms = ?age_ms,
        "Session cancelled"
    );

    Json(CancelSessionResponse {
        success: true,
    })
}
