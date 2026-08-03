mod acknowledge_group_warnings;
mod analyze;
mod auth_invites;
mod auth_login;
mod auth_logout;
mod auth_register;
mod auth_totp;
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
use std::time::Duration;

use axum::{
    body::Body,
    extract::{DefaultBodyLimit, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};

use serde::Serialize;

use tower_governor::{governor::GovernorConfigBuilder, GovernorError, GovernorLayer};
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

    // Ephemeral WebAuthn registration-ceremony state (see
    // auth::RegistrationCeremony) -- same generic SessionStore engine as
    // unit_group_sessions/dedup_sessions above, just a much shorter
    // timeout, since a ceremony is one request/response round trip, not
    // a standing session.
    pub registration_ceremonies: Arc<dyn SessionStore<crate::auth::RegistrationCeremony>>,

    // Login's counterpart to registration_ceremonies. A separate store,
    // not a shared one: the two hold different webauthn-rs state types and
    // can be in flight simultaneously (see the ceremony-cookie names in
    // auth/ceremony_cookie.rs for the same reasoning).
    pub authentication_ceremonies: Arc<dyn SessionStore<crate::auth::AuthenticationCeremony>>,
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

    // Rate limit for the endpoints an anonymous caller can reach without
    // ever having a valid session: passkey registration (both the
    // invite-redemption and add-a-second-key paths), passkey login, and
    // the TOTP fallback login. One shared bucket across all of them,
    // keyed by peer IP -- deliberately not one bucket per route, so a
    // script cannot get five times the budget just by spreading its
    // attempts across five endpoints instead of one.
    //
    // Ten requests answered immediately, one more every three seconds
    // after that (~20/min sustained). Generous enough that a real person
    // retrying a cancelled Windows Hello prompt or fumbling a TOTP code a
    // few times in a row never notices this exists, while bounding how
    // fast an anonymous caller can iterate through addresses or guess
    // codes against these endpoints.
    //
    // Keying is by the TCP peer address (`tower_governor`'s default
    // `PeerIpKeyExtractor`), never a client-supplied header -- this
    // deliberately does not attempt to trust `X-Forwarded-For`, since no
    // trusted-reverse-proxy policy exists yet (see the `ip_address` NULL
    // comments in auth_register.rs / auth_login.rs for the same open
    // question). Once real client IPs need trusting for any reason, this
    // and that NULL should be revisited together, not separately -- they
    // are the same unresolved question in two places. Until then, behind
    // a reverse proxy that does not preserve the original TCP peer, this
    // still limits correctly, just coarsely: every client behind that
    // proxy shares one bucket rather than getting one each, which is
    // strictly more restrictive than intended, never less.
    let auth_rate_limit = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(3)
            .burst_size(10)
            .finish()
            .expect("auth rate-limit config: burst size and period are both non-zero constants"),
    );

    // A separate, more generous bucket for invite creation: authenticated
    // and admin-only already, so this is bounding accidental or scripted
    // hammering by a trusted caller, not probing by an anonymous one.
    let invite_rate_limit = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(2)
            .burst_size(20)
            .finish()
            .expect("invite rate-limit config: burst size and period are both non-zero constants"),
    );

    // The keyed limiter accumulates one entry per distinct peer IP it has
    // ever seen and nothing prunes that on its own -- `retain_recent()` is
    // `governor`'s own answer, and it has to be called from somewhere.
    // Mirrors `InMemorySessionStore::start_cleanup_task`: a background
    // tick that must keep running even if one iteration panics, since the
    // alternative is the rate limiter quietly becoming a slow memory leak
    // for the life of the process.
    {
        let auth_limiter = auth_rate_limit.limiter().clone();
        let invite_limiter = invite_rate_limit.limiter().clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));

            loop {
                interval.tick().await;
                auth_limiter.retain_recent();
                invite_limiter.retain_recent();
            }
        });
    }

    // Split out as their own routers purely so the rate-limit layer
    // applies to exactly these paths and nothing else -- merged back into
    // the main router below while it is still `Router<AppState>`, since
    // `.merge` requires matching state types and `.with_state` further
    // down converts the main chain to `Router<()>`.
    let auth_routes = Router::new()
        .route("/auth/register/begin", post(auth_register::register_begin))
        .route(
            "/auth/register/finish",
            post(auth_register::register_finish),
        )
        .route("/auth/login/begin", post(auth_login::login_begin))
        .route("/auth/login/finish", post(auth_login::login_finish))
        .route("/auth/login/totp", post(auth_totp::login))
        .layer(GovernorLayer::new(auth_rate_limit).error_handler(rate_limit_exceeded));

    let invite_routes = Router::new()
        // Admin-only. Authorization is the `AuthenticatedUser` extractor in
        // the handler plus the admin-only RLS policies underneath it, not a
        // route-level guard -- there is no middleware layer that could be
        // reordered away from this path.
        .route("/auth/invites", post(auth_invites::create_invite))
        // Account recovery shares this bucket rather than the anonymous
        // auth_routes one above -- same trust level as invite creation
        // (authenticated admin), same "bound accidental/scripted
        // hammering by a trusted caller" rationale.
        .route("/auth/invites/recover", post(auth_invites::recover_account))
        .layer(GovernorLayer::new(invite_rate_limit).error_handler(rate_limit_exceeded));

    Router::new()
        .route("/health", get(health))
        .route("/health/db", get(health_db))
        .route("/health/whoami", get(whoami))
        // Deliberately NOT behind the AuthenticatedUser extractor: signing
        // out must succeed with a stale or missing cookie, or the one case
        // where a user most needs to clear it is the case that 401s. See
        // auth_logout's module docs.
        // TOTP: enrolment and removal are authenticated (the extractor is
        // in the handler); the sign-in path is not, and is deliberately as
        // opaque as the passkey login path -- and merged in below (see
        // auth_routes) with the other unauthenticated auth endpoints,
        // sharing their rate limit.
        .route("/auth/totp/enroll/begin", post(auth_totp::enroll_begin))
        .route("/auth/totp/enroll/confirm", post(auth_totp::enroll_confirm))
        .route("/auth/totp/disable", post(auth_totp::disable))
        .route("/auth/logout", post(auth_logout::logout))
        .route(
            "/auth/logout/everywhere",
            post(auth_logout::logout_everywhere),
        )
        .merge(auth_routes)
        .merge(invite_routes)
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
        // A request that never reaches a handler at all -- malformed
        // JSON, the wrong Content-Type, or a body over DefaultBodyLimit
        // above -- is rejected by axum's own `Json<T>` extractor with a
        // plain-text body, not this project's `ApiErrorBody` shape every
        // handler-level error already uses. Every other error path in
        // this API (`session_not_found`, `stage_conflict`,
        // `internal_error`, and each handler's own structured responses)
        // is `{error, message}` JSON; a client parsing that consistently
        // would mishandle these three plain-text cases. This layer
        // rewrites them to match after the fact rather than changing
        // every handler's extractor type, which would be a much larger,
        // purely mechanical change for the same outcome.
        .layer(middleware::from_fn(normalize_extraction_rejection_body))
        // Outermost layer — catches a panic anywhere in the stack below
        // (routes, cors, body-limit) and turns it into the project's own
        // ApiErrorBody 500 shape instead of silently dropping the
        // connection with no response at all.
        .layer(CatchPanicLayer::custom(handle_panic))
}

/// `tower_governor`'s own default rejection is plain text (e.g. `"Too Many
/// Requests! Wait for 3s"`), which is exactly the inconsistency
/// `normalize_extraction_rejection_body` above already exists to close for
/// a different auto-generated rejection class. Rather than reintroduce a
/// third response shape, this maps a governor rejection onto the same
/// `ApiErrorBody` every handler-level error already uses.
fn rate_limit_exceeded(error: GovernorError) -> Response {
    match error {
        GovernorError::TooManyRequests { wait_time, headers } => {
            let mut response = (
                StatusCode::TOO_MANY_REQUESTS,
                Json(ApiErrorBody {
                    error: "rate_limited",
                    message: format!("Too many requests. Try again in {wait_time} second(s)."),
                }),
            )
                .into_response();

            if let Some(headers) = headers {
                response.headers_mut().extend(headers);
            }

            response
        }

        // Both are effectively "the rate limiter itself is misconfigured
        // or malfunctioning" rather than anything about the caller's
        // request, so they get the project's own internal_error path
        // instead of inventing a fourth shape for a case that should not
        // occur -- `UnableToExtractKey` cannot happen with the peer-IP
        // extractor used here (it never fails to extract), and `Other` is
        // never constructed by anything in this codebase.
        GovernorError::UnableToExtractKey | GovernorError::Other { .. } => {
            tracing::error!(?error, "rate limiter returned an unexpected error");
            internal_error("Could not process this request")
        }
    }
}

/// See the doc comment on its `.layer(...)` call site in `router` above.
/// Only rewrites a response that (a) has one of the three status codes
/// axum's built-in extractors/body-limit actually produce for this
/// failure class, and (b) isn't already JSON -- a handler's own
/// legitimately-JSON 400 (e.g. `stage_conflict`, `correct_group`'s
/// `unknown_group`) must pass through completely untouched.
async fn normalize_extraction_rejection_body(request: Request, next: Next) -> Response {
    let response = next.run(request).await;

    let status = response.status();

    if !matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::PAYLOAD_TOO_LARGE
    ) {
        return response;
    }

    let already_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));

    if already_json {
        return response;
    }

    let (parts, body) = response.into_parts();

    let message = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(_) => return Response::from_parts(parts, Body::empty()),
    };

    let error = match parts.status {
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
        StatusCode::PAYLOAD_TOO_LARGE => "payload_too_large",
        _ => "invalid_request_body",
    };

    (parts.status, Json(ApiErrorBody { error, message })).into_response()
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
