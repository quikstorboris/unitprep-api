mod acknowledge_group_warnings;
mod analyze;
mod auth_audit_logs;
mod auth_audit_logs_export;
mod auth_configuration;
mod auth_invites;
mod auth_login;
mod auth_logout;
mod auth_passkey_reverify;
mod auth_register;
mod auth_roles;
mod auth_totp;
mod auth_user_role;
mod auth_user_status;
mod auth_users;
mod cancel_session;
mod client_ops_activity_logs;
mod client_ops_activity_logs_export;
mod client_ops_qms_tags;
mod clients_companies;
mod clients_create;
mod clients_detail;
mod clients_elavon;
mod clients_facility_people;
mod clients_preview;
mod clients_resync;
mod clients_search;
mod clients_sync;
mod correct;
mod correct_group;
mod dedup;
mod dedup_view;
pub(crate) mod discover;
mod dropbox_browse;
mod exclude_group;
mod exclude_groups;
mod exempt;
mod export;
mod group_file_confirm;
mod group_file_upload;
mod health;
mod manual_file_upload;
mod process_street_settings;
mod resolve_unit_format;
mod router;
mod select_group_file;
pub(crate) mod select_unit_file;
mod state;
mod tagger;
mod unit_file_upload;
mod upload;
pub(crate) mod validate;

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

use serde::Serialize;

pub use router::router;
pub use state::AppState;

/// The one true "your session is gone" response — a session can disappear
/// either because it expired (10-minute idle timeout) or because the id
/// was never valid. Every endpoint that looks up a session by id should
/// return this instead of silently faking a zero-value success response,
/// so the frontend can distinguish "nothing to report" from "there's
/// nothing here to report on" and show an explicit expired-session screen
/// rather than a confusing all-zeros result.
pub(crate) fn session_not_found(session_id: &str) -> Response {
    // Logged here, not left to each of the ~20 call sites to remember --
    // before this, whether a session-not-found ever left a trace depended
    // entirely on whether that specific handler happened to log first.
    tracing::warn!(session_id = %session_id, "session not found or expired");

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
    session_id: &str,
    err: crate::application::unit_group_session::StageError,
) -> Response {
    // Same reasoning as session_not_found's own log line above -- some
    // call sites already warn with richer, handler-specific context
    // before reaching here (a file name, a unit number); this one is
    // guaranteed regardless, so a caller that forgets to still leaves a
    // trace.
    tracing::warn!(
        session_id = %session_id,
        required = ?err.required,
        current = ?err.current,
        "stage conflict"
    );

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
    session_id: &str,
    result: Option<Result<T, crate::application::unit_group_session::StageError>>,
) -> Response {
    match result {
        Some(Ok(body)) => Json(body).into_response(),
        Some(Err(err)) => stage_conflict(session_id, err),
        None => session_not_found(session_id),
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

/// The browser/IP pair nearly every auth handler pulls out of the request
/// to pass into `audit_log::record` -- `user_agent` borrows from `headers`,
/// which is why this takes a reference rather than an owned `HeaderMap`.
/// Handlers with no `ConnectInfo<SocketAddr>` extractor on their route (no
/// IP to report) use `user_agent_from` alone instead of this.
pub(crate) fn request_context(
    headers: &axum::http::HeaderMap,
    addr: std::net::SocketAddr,
) -> (Option<&str>, Option<sqlx::types::ipnetwork::IpNetwork>) {
    (
        user_agent_from(headers),
        Some(sqlx::types::ipnetwork::IpNetwork::from(addr.ip())),
    )
}

/// Just the `User-Agent` half of `request_context`, for handlers whose
/// route has no `ConnectInfo<SocketAddr>` extractor to pair it with.
pub(crate) fn user_agent_from(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
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

#[cfg(test)]
#[path = "tagger_test_support.rs"]
pub(crate) mod tagger_test_support;

/// Real HTTP-level tests, complementing (not replacing) the direct-call
/// style above. Some bugs only exist at the router/middleware layer --
/// the CORS credentials gap this suite regression-tests was invisible to
/// every direct handler call, since `CorsLayer` never runs at all unless
/// a request actually goes through the real `Router`.
#[cfg(test)]
#[path = "http_integration_tests.rs"]
mod http_integration_tests;
