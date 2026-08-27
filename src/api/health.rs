use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use unitprep_core::session_store::SessionMetrics;

use super::{internal_error, AppState};

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
    let mut tx = crate::auth::begin_owner_rls_transaction(&state.db, user.user_id)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, user_id = %user.user_id, "whoami: failed to open transaction");
            internal_error("Could not look up your account")
        })?;

    let totp_enrolled: bool = sqlx::query_scalar(
        "SELECT confirmed_at IS NOT NULL FROM auth.totp_credentials WHERE user_id = $1",
    )
    .bind(user.user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(error = %err, user_id = %user.user_id, "whoami: totp lookup failed");
        internal_error("Could not look up your account")
    })?
    .unwrap_or(false);

    let (first_name, last_name): (String, String) =
        sqlx::query_as("SELECT first_name, last_name FROM auth.users WHERE id = $1")
            .bind(user.user_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|err| {
                tracing::error!(error = %err, user_id = %user.user_id, "whoami: name lookup failed");
                internal_error("Could not look up your account")
            })?;

    tx.commit().await.map_err(|err| {
        tracing::error!(error = %err, user_id = %user.user_id, "whoami: commit failed");
        internal_error("Could not look up your account")
    })?;

    Ok(Json(WhoamiResponse {
        user_id: user.user_id.to_string(),
        first_name,
        last_name,
        roles: user.role_keys.clone(),
        permissions: user.permission_keys.iter().cloned().collect(),
        totp_enrolled,
        step_up_required: user.requires_step_up,
    }))
}
