mod acknowledge_group_warnings;
mod analyze;
mod cancel_session;
mod correct;
mod correct_group;
mod dedup;
mod dedup_view;
pub(crate) mod discover;
mod exclude_group;
mod exclude_groups;
mod exempt;
mod export;
mod group_file_confirm;
mod group_file_upload;
mod manual_file_upload;
mod resolve_unit_format;
mod select_group_file;
pub(crate) mod select_unit_file;
mod upload;
pub(crate) mod validate;

use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};

use serde::Serialize;

use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};

use unitprep_core::session_store::{SessionMetrics, SessionStore};

use crate::application::dedup_session_service::DedupSession;
use crate::application::unit_group_session::Session;

#[derive(Clone)]
pub struct AppState {
    // Named for the tool it serves, not just "the store" — UnitPrep is
    // moving toward multiple tools each with their own session type and
    // their own store instance (see unitprep-core's generic
    // SessionStore<S>); this field will get company (e.g.
    // `dedup_sessions`) rather than being renamed later under pressure.
    pub unit_group_sessions: Arc<dyn SessionStore<Session>>,

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
pub(crate) fn stage_conflict(err: crate::application::unit_group_session::StageError) -> Response {
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

/// Shared response shape for the common "stage-gated mutation" pattern:
/// `Some(Ok(_))` → 200 with the JSON body, `Some(Err(_))` → 409
/// (`stage_conflict`), `None` → 404 (`session_not_found`). Several
/// handlers (correct, exempt, exclude-group(s), acknowledge-group-
/// warnings) shared this exact three-arm match as their own separately
/// written copy — this collapses each into one call. Handlers whose
/// inner error type carries more than a bare `StageError` (e.g.
/// `correct_group`'s `UnknownGroup` variant) keep their own custom match
/// instead of forcing an unrelated variant through this shape.
pub(crate) fn respond<T: Serialize>(
    result: Option<Result<T, crate::application::unit_group_session::StageError>>,
) -> Response {
    match result {
        Some(Ok(body)) => Json(body).into_response(),
        Some(Err(err)) => stage_conflict(err),
        None => session_not_found(),
    }
}

/// A genuine internal failure while processing an otherwise-valid
/// request (not a data-quality or stage problem) — a 500. `context`
/// should be a short, safe-to-display description; the real error detail
/// belongs in the `tracing::error!` call the caller already makes
/// alongside this, not in the response body.
pub(crate) fn internal_error(context: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiErrorBody {
            error: "internal_error",
            message: format!("{context} — check server logs for details.",),
        }),
    )
        .into_response()
}

/// Origins allowed to call this API. Defaults to the frontend dev servers
/// so local development needs no configuration; set
/// `CORS_ALLOWED_ORIGINS` (comma-separated) to add real deployed
/// frontend origins instead of hardcoding them here.
fn allowed_origins() -> Vec<axum::http::HeaderValue> {
    match std::env::var("CORS_ALLOWED_ORIGINS") {
        Ok(value) if !value.trim().is_empty() => value
            .split(',')
            .map(|origin| origin.trim())
            .filter(|origin| !origin.is_empty())
            .filter_map(|origin| origin.parse().ok())
            .collect(),

        _ => vec![
            "http://localhost:3000".parse().unwrap(),
            "http://localhost:5173".parse().unwrap(),
        ],
    }
}

pub fn router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins()))
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
        .allow_headers([axum::http::header::CONTENT_TYPE])
        // The frontend's shared hooks (useSessionPost/useSessionAction)
        // now send `credentials: "include"` on every request, ahead of
        // auth actually issuing a session cookie -- per the Fetch/CORS
        // spec, a credentialed request's response is invisible to the
        // browser unless the server explicitly echoes this header, even
        // before any real cookie exists to send. `allow_origin` above is
        // already a specific list (never `*`), which credentialed CORS
        // requires regardless.
        .allow_credentials(true);

    Router::new()
        .route("/health", get(health))
        .route("/health/db", get(health_db))
        .route("/health/whoami", get(whoami))
        .route("/upload", post(upload::upload))
        .route("/discover", post(discover::discover))
        .route("/validate", post(validate::validate))
        .route("/correct", post(correct::correct))
        .route("/correct-group", post(correct_group::correct_group))
        .route("/exempt-dimensions", post(exempt::exempt_dimensions))
        .route("/exclude-group", post(exclude_group::exclude_group))
        .route("/exclude-groups", post(exclude_groups::exclude_groups))
        .route(
            "/acknowledge-group-warnings",
            post(acknowledge_group_warnings::acknowledge_group_warnings),
        )
        .route("/analyze", post(analyze::analyze))
        .route("/export", post(export::export))
        .route(
            "/unit-file/select",
            post(select_unit_file::select_unit_file),
        )
        .route(
            "/unit-file/resolve-format",
            post(resolve_unit_format::resolve_unit_format),
        )
        .route(
            "/group-file/upload",
            post(group_file_upload::upload_group_file),
        )
        .route(
            "/group-file/confirm",
            post(group_file_confirm::confirm_group_file),
        )
        .route(
            "/group-file/select",
            post(select_group_file::select_group_file),
        )
        .route("/session/cancel", post(cancel_session::cancel_session))
        .route("/dedup/check", post(dedup::check))
        .route("/dedup/report", post(dedup::report))
        .route("/dedup/export", post(dedup::export))
        .layer(DefaultBodyLimit::max(100 * 1024 * 1024))
        .with_state(state)
        .layer(cors)
        // Outermost layer — catches a panic anywhere in the stack below
        // (routes, cors, body-limit) and turns it into the project's own
        // ApiErrorBody 500 shape instead of silently dropping the
        // connection with no response at all.
        .layer(CatchPanicLayer::custom(handle_panic))
}

/// Turns a caught handler panic into a logged event plus the project's
/// standard `internal_error` response — the real panic detail goes to
/// the server log via `tracing::error!`, never into the response body a
/// client sees.
fn handle_panic(err: Box<dyn std::any::Any + Send + 'static>) -> Response {
    let message = if let Some(s) = err.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = err.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    };

    tracing::error!(
        panic_message = %message,
        "request handler panicked"
    );

    internal_error("The server encountered an unexpected error")
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    sessions: SessionMetrics,
    dedup_sessions: SessionMetrics,
}

async fn health(State(state): State<AppState>) -> Json<HealthResponse> {
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
async fn health_db(State(state): State<AppState>) -> Response {
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
struct WhoamiResponse {
    user_id: String,
    role: &'static str,
}

/// Manual/diagnostic verification that the whole cookie -> resolve_session
/// -> identity chain actually works end to end -- exercises
/// AuthenticatedUser the same way any future protected endpoint will,
/// without yet having a real protected endpoint to exercise it through.
async fn whoami(user: crate::auth::AuthenticatedUser) -> Json<WhoamiResponse> {
    Json(WhoamiResponse {
        user_id: user.user_id.to_string(),
        role: user.role.as_db_text(),
    })
}

#[cfg(test)]
mod panic_handler_tests {
    use super::*;

    #[test]
    fn handle_panic_returns_a_500_for_a_str_payload() {
        let response = handle_panic(Box::new("boom"));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn handle_panic_returns_a_500_for_a_string_payload() {
        let response = handle_panic(Box::new(String::from("boom")));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// A panic payload isn't always a &str/String (`std::panic::panic_any`
    /// can carry anything) — the fallback branch must still produce a
    /// clean 500, not panic itself while handling a panic.
    #[test]
    fn handle_panic_returns_a_500_for_an_unrecognized_payload() {
        let response = handle_panic(Box::new(42_i32));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
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

/// Real HTTP-level tests, complementing (not replacing) the direct-call
/// style above. Some bugs only exist at the router/middleware layer --
/// the CORS credentials gap this suite regression-tests was invisible to
/// every direct handler call, since `CorsLayer` never runs at all unless
/// a request actually goes through the real `Router`.
#[cfg(test)]
#[path = "http_integration_tests.rs"]
mod http_integration_tests;
