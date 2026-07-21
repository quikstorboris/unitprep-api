mod analyze;
mod cancel_session;
mod correct;
mod dedup;
mod discover;
mod exempt;
mod export;
mod select_group_file;
mod upload;
pub(crate) mod validate;

use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json,
    Router,
};

use serde::Serialize;

use tower_http::cors::{
    AllowOrigin,
    CorsLayer,
};

use unitprep_core::session_store::{
    SessionMetrics,
    SessionStore,
};

use crate::application::dedup_session_service::DedupSession;
use crate::application::unit_group_session::Session;

#[derive(Clone)]
pub struct AppState {
    // Named for the tool it serves, not just "the store" — UnitPrep is
    // moving toward multiple tools each with their own session type and
    // their own store instance (see unitprep-core's generic
    // SessionStore<S>); this field will get company (e.g.
    // `dedup_sessions`) rather than being renamed later under pressure.
    pub unit_group_sessions:
        Arc<dyn SessionStore<Session>>,

    // Additive, per the comment above — a second tool's store, not a
    // rename of the first.
    pub dedup_sessions: Arc<dyn SessionStore<DedupSession>>,

    // The app_service-authenticated connection pool -- see db.rs for
    // why it is built lazily rather than blocking startup on Postgres
    // being reachable.
    pub db: sqlx::PgPool,

    // See auth/mod.rs for the AuthBackend trait -- Arc<dyn ...>, same
    // pattern as the session stores above, so a future backend swap is
    // a new impl, not a rewrite of every call site.
    pub auth_backend: Arc<dyn crate::auth::AuthBackend>,
}

/// The one true "your session is gone" response — a session can disappear
/// either because it expired (10-minute idle timeout) or because the id
/// was never valid. Every endpoint that looks up a session by id should
/// return this instead of silently faking a zero-value success response,
/// so the frontend can distinguish "nothing to report" from "there's
/// nothing here to report on" and show an explicit expired-session screen
/// rather than a confusing all-zeros result.
pub(crate) fn session_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ApiErrorBody {
            error: "session_not_found",
            message: "Session not found or expired".to_string(),
        }),
    )
        .into_response()
}

/// Structured shape for every non-2xx JSON error response below —
/// `error` is a stable, machine-matchable code (for a frontend or a
/// second tool's client to branch on); `message` is the human-readable
/// detail, safe to display as-is.
#[derive(Serialize)]
pub(crate) struct ApiErrorBody {
    pub error: &'static str,
    pub message: String,
}

/// The session exists but hasn't reached the workflow stage this action
/// requires yet (e.g. calling `/analyze` before `/validate` has
/// completed) — a 409 Conflict: the request is well-formed and the
/// session is real, it's just not in the right state yet. Distinct from
/// both "session missing" (404, `session_not_found`) and a genuine
/// internal failure (500, `internal_error`).
///
/// Previously, endpoints returned a fake all-zero 200 success for this
/// case — indistinguishable from a legitimately empty (but real) result,
/// which is exactly the ambiguity `session_not_found`'s own doc comment
/// above already identifies as the thing to avoid. This closes that same
/// gap for stage violations.
pub(crate) fn stage_conflict(
    err: crate::application::unit_group_session::StageError,
) -> Response {
    (
        StatusCode::CONFLICT,
        Json(ApiErrorBody {
            error: "stage_conflict",
            message: format!(
                "This action requires the session to have reached the {:?} stage; it is currently at {:?}.",
                err.required, err.current
            ),
        }),
    )
        .into_response()
}

/// A genuine internal failure while processing an otherwise-valid
/// request (not a data-quality or stage problem) — a 500. `context`
/// should be a short, safe-to-display description; the real error detail
/// belongs in the `tracing::error!` call the caller already makes
/// alongside this, not in the response body.
pub(crate) fn internal_error(
    context: &str,
) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiErrorBody {
            error: "internal_error",
            message: format!(
                "{context} — check server logs for details.",
            ),
        }),
    )
        .into_response()
}

/// Origins allowed to call this API. Defaults to the frontend dev servers
/// so local development needs no configuration; set
/// `CORS_ALLOWED_ORIGINS` (comma-separated) to add real deployed
/// frontend origins instead of hardcoding them here.
fn allowed_origins() -> Vec<axum::http::HeaderValue> {
    match std::env::var("CORS_ALLOWED_ORIGINS")
    {
        Ok(value)
            if !value.trim().is_empty() =>
        {
            value
                .split(',')
                .map(|origin| {
                    origin.trim()
                })
                .filter(|origin| {
                    !origin.is_empty()
                })
                .filter_map(|origin| {
                    origin.parse().ok()
                })
                .collect()
        }

        _ => vec![
            "http://localhost:3000"
                .parse()
                .unwrap(),
            "http://localhost:5173"
                .parse()
                .unwrap(),
        ],
    }
}

pub fn router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(
            allowed_origins(),
        ))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
        ]);

    Router::new()
        .route("/health", get(health))
        .route("/health/db", get(health_db))
        .route("/upload", post(upload::upload))
        .route("/discover", post(discover::discover))
        .route("/validate", post(validate::validate))
        .route("/correct", post(correct::correct))
        .route(
            "/exempt-dimensions",
            post(exempt::exempt_dimensions),
        )
        .route("/analyze", post(analyze::analyze))
        .route("/export", post(export::export))
        .route(
            "/group-file/select",
            post(select_group_file::select_group_file),
        )
        .route(
            "/session/cancel",
            post(cancel_session::cancel_session),
        )
        .route("/dedup/check", post(dedup::check))
        .route("/dedup/report", post(dedup::report))
        .route("/dedup/export", post(dedup::export))
        .layer(
            DefaultBodyLimit::max(
                100 * 1024 * 1024,
            ),
        )
        .with_state(state)
        .layer(cors)
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    sessions: SessionMetrics,
    dedup_sessions: SessionMetrics,
}

async fn health(
    State(state): State<AppState>,
) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        // Read from Cargo.toml at compile time — bumping the version
        // there is the only thing needed to keep this in sync; nothing
        // to remember to update in two places.
        version: env!(
            "CARGO_PKG_VERSION"
        ),
        sessions: state
            .unit_group_sessions
            .metrics(),
        dedup_sessions: state
            .dedup_sessions
            .metrics(),
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
async fn health_db(
    State(state): State<AppState>,
) -> Response {
    match sqlx::query_scalar::<_, String>(
        "SELECT current_user",
    )
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
            internal_error(
                "Database connectivity check failed",
            )
        }
    }
}

/// Shared session-construction helpers for endpoint-level tests. Handlers
/// are called directly (`handler(State(state), Json(request)).await`)
/// rather than through a live HTTP router — `State`/`Json` are plain
/// public tuple structs, so this exercises the real handler logic
/// (session lookup, stage checks, response codes) without needing to
/// fabricate multipart bodies or spin up a server.
#[cfg(test)]
#[path = "test_support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "dedup_test_support.rs"]
pub(crate) mod dedup_test_support;
