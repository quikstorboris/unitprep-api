use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use unitprep_core::session_store::SessionMetrics;

use super::{error_response, internal_error, AppState};

#[derive(Serialize)]
pub(super) struct HealthResponse {
    status: &'static str,
    version: &'static str,
    sessions: SessionMetrics,
    dedup_sessions: SessionMetrics,
}

pub(super) async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        // Read from Cargo.toml at compile time — bumping the version
        // there is the only thing needed to keep this in sync; nothing
        // to remember to update in two places.
        version: env!("CARGO_PKG_VERSION"),
        sessions: state.unit_group_sessions.metrics(),
        dedup_sessions: state.dedup_sessions.metrics(),
    })
}

#[derive(Serialize)]
struct DbHealthResponse {
    status: &'static str,
    connected_as: String,
}

/// Confirms the database pool is actually reachable and -- just as
/// importantly -- authenticating as the expected app_service role, not
/// the migration/owner role. Pasting the wrong connection string into
/// DATABASE_URL (e.g. the owner's direct URL instead of app_service's)
/// would otherwise silently bypass every RLS policy in the schema while
/// still working from the app's point of view, so this check is
/// deliberately more than a bare SELECT 1.
pub(super) async fn health_db(State(state): State<AppState>) -> Response {
    match sqlx::query_scalar::<_, String>("SELECT current_user")
        .fetch_one(&state.db)
        .await
    {
        Ok(connected_as) => (
            StatusCode::OK,
            Json(DbHealthResponse {
                status: "ok",
                connected_as,
            }),
        )
            .into_response(),
        Err(err) => {
            tracing::error!(
                error = %err,
                "database health check failed"
            );
            internal_error("Database connectivity check failed")
        }
    }
}

#[derive(Serialize)]
pub(super) struct WhoamiResponse {
    user_id: String,
    first_name: String,
    last_name: String,
    roles: Vec<String>,
    permissions: Vec<String>,
    /// Whether a *confirmed* TOTP credential exists for this account --
    /// lets the frontend show "enrolled" vs. a call-to-action instead of
    /// always presenting "enroll", which would walk an already-enrolled
    /// user into silently replacing their working fallback (re-enrolling
    /// overwrites the secret immediately, see auth_totp.rs).
    totp_enrolled: bool,
    /// True when this session is pending a login-time step-up (Phase II
    /// anomaly signal -- see `AuthenticatedUser`'s `STEP_UP_ALLOWED_PATHS`
    /// and auth_login.rs's `assess_login_risk`). `whoami` is one of the
    /// few routes reachable while this is true, specifically so the
    /// frontend can tell "signed in but pending step-up" apart from "not
    /// signed in at all" instead of inferring it from every other route
    /// 403ing.
    step_up_required: bool,
}

/// Manual/diagnostic verification that the whole cookie -> resolve_session
/// -> identity chain actually works end to end -- exercises
/// AuthenticatedUser the same way any future protected endpoint will,
/// without yet having a real protected endpoint to exercise it through.
///
/// Also the frontend's one source of truth for current-user state, so it
/// carries the `totp_enrolled` flag alongside identity rather than needing
/// a second round trip just to render the account/security page correctly.
pub(super) async fn whoami(
    State(state): State<AppState>,
    user: crate::auth::AuthenticatedUser,
) -> Result<Json<WhoamiResponse>, Response> {
    // One statement, not an RLS transaction of five -- each statement is a
    // network round trip, and every page load waits on this call. See
    // `auth.session_profile` (migration `20261029160000`) for why this is
    // safe: it is keyed by this request's own session token and decides
    // session validity through `resolve_session`, so it can only describe
    // the signed-in user.
    let profile = crate::auth::query_session_profile(&user.token_hash, &state.db)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, user_id = %user.user_id, "whoami: profile lookup failed");
            internal_error("Could not look up your account")
        })?
        .ok_or_else(|| {
            // The session stopped being valid between the extractor's check
            // and this read (revoked, expired, user deactivated).
            error_response(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Sign in required".to_string(),
            )
        })?;

    Ok(Json(WhoamiResponse {
        user_id: user.user_id.to_string(),
        first_name: profile.first_name,
        last_name: profile.last_name,
        roles: user.role_keys.clone(),
        permissions: user.permission_keys.iter().cloned().collect(),
        totp_enrolled: profile.totp_enrolled,
        step_up_required: user.requires_step_up,
    }))
}
