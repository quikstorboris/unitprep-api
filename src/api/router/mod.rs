use std::time::Duration;

use axum::{
    body::Body,
    extract::Request,
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Json, Router,
};

use tower_governor::GovernorError;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::request_id::{
    MakeRequestUuid, PropagateRequestIdLayer, RequestId, SetRequestIdLayer,
};
use tower_http::trace::TraceLayer;

use super::{internal_error, ApiErrorBody, AppState};

/// The full route table (every path, paired with the `RouteAccess` that
/// authorizes it) -- split out on its own, see that file's own module doc
/// for why.
mod routes;

/// The runtime proof that every `RouteAccess::Permission` route `routes`
/// declares is actually enforced by the handler wired up to it -- split
/// out on its own since it grew alongside `routes` into the same "this
/// file is now two concerns" problem. See its own module doc.
#[cfg(test)]
mod permission_gate_tests;

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

/// One id per request, threaded through every log line emitted while
/// handling it (via the `TraceLayer` span below) and echoed back on the
/// response so a user reporting an issue can quote the exact request --
/// answering "what happened for this click" without cross-referencing
/// timestamps across possibly-concurrent requests. `x-request-id` is the
/// de facto standard header name for this.
static REQUEST_ID_HEADER: header::HeaderName = header::HeaderName::from_static("x-request-id");

pub fn router(state: AppState) -> Router {
    with_response_layers(routes::build(state).into_parts().0)
}

/// Response-shaping middleware, applied to the state-erased `Router<()>`
/// -- none of this affects authorization, so it lives outside `build`
/// and outside the `GatedRouter` manifest entirely.
fn with_response_layers(router: Router) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(allowed_origins()))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
        ])
        .allow_headers([axum::http::header::CONTENT_TYPE])
        // The frontend's shared hooks (useSessionPost/useSessionAction)
        // now send `credentials: "include"` on every request, ahead of
        // auth actually issuing a session cookie -- per the Fetch/CORS
        // spec, a credentialed request's response is invisible to the
        // browser unless the server explicitly echoes this header, even
        // before any real cookie exists to send. `allow_origin` above is
        // already a specific list (never `*`), which credentialed CORS
        // requires regardless.
        .allow_credentials(true)
        // Content-Disposition is not a CORS-safelisted response header,
        // so without this, every file-download endpoint's
        // `response.headers.get("Content-Disposition")` on the frontend
        // (dedup/audit-log/user export, tagger apply -- every one of
        // downloadBlob's callers) silently reads null and falls back to
        // its hardcoded default filename, even though the real header
        // is present on the wire. Same class of gap as the PUT/PATCH
        // CORS fix above: a browser-only restriction with no server-side
        // symptom, so it's invisible unless a download's real filename
        // is deliberately checked against something other than its own
        // fallback.
        .expose_headers([axum::http::header::CONTENT_DISPOSITION]);

    router
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
        // Catches a panic anywhere in the stack below (routes, cors,
        // body-limit) and turns it into the project's own ApiErrorBody
        // 500 shape instead of silently dropping the connection with no
        // response at all. No longer literally the outermost layer (the
        // three request-id/trace layers below wrap it), but still the
        // outermost of the response-shaping ones.
        .layer(CatchPanicLayer::custom(handle_panic))
        // Copies the id `SetRequestIdLayer` below assigned back onto the
        // response header, once a response exists -- applied here (more
        // inner than TraceLayer) so it runs before TraceLayer's own
        // on_response sees the response, per tower-http's documented
        // request-id composition.
        .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER.clone()))
        // The span this creates wraps every handler/layer below it, so
        // every `tracing::` call made while handling a request inherits
        // `request_id`/`method`/`path` as span context automatically --
        // no need to thread the id through each handler by hand.
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request| {
                    let request_id = request
                        .extensions()
                        .get::<RequestId>()
                        .and_then(|id| id.header_value().to_str().ok())
                        .unwrap_or("unknown")
                        .to_string();

                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        path = %request.uri().path(),
                        request_id = %request_id,
                    )
                })
                .on_response(
                    |response: &Response, latency: Duration, _span: &tracing::Span| {
                        // Read back off the response rather than threading
                        // the id through separately -- PropagateRequestIdLayer
                        // (more inner, so it runs first on the way out) has
                        // already copied it onto this exact response by the
                        // time this fires.
                        let request_id = response
                            .headers()
                            .get(&REQUEST_ID_HEADER)
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("unknown");

                        tracing::info!(
                            request_id = %request_id,
                            status = response.status().as_u16(),
                            latency_ms = latency.as_millis(),
                            "request completed"
                        );
                    },
                ),
        )
        // Outermost layer overall -- assigns the id before anything else
        // (cors, body-limit, catch-panic, every route) sees the request,
        // so every request gets one regardless of how it's ultimately
        // handled or rejected.
        .layer(SetRequestIdLayer::new(
            REQUEST_ID_HEADER.clone(),
            MakeRequestUuid,
        ))
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

/// Wraps `rate_limit_exceeded` with an audit row for the one case that is
/// actually about the caller -- `TooManyRequests`. `tower_governor`'s
/// `error_handler` only receives the `GovernorError`, not the original
/// request, so there is no `ConnectInfo` to bind here; `bucket` (`"auth"`
/// or `"invite"`) is what distinguishes which limiter tripped.
///
/// The handler itself stays synchronous (that is what `error_handler`
/// requires), so the write is fire-and-forget on a spawned task rather
/// than awaited in place -- the same "must not affect the response"
/// property `audit_log::record` already has, just reached a different way
/// here since this function cannot itself be `async`.
fn rate_limit_exceeded_with_audit(
    bucket: &'static str,
    db: sqlx::PgPool,
) -> impl Fn(GovernorError) -> Response + Clone + Send + Sync + 'static {
    move |error: GovernorError| {
        if matches!(error, GovernorError::TooManyRequests { .. }) {
            let db = db.clone();
            tokio::spawn(async move {
                crate::auth::audit_log::record(
                    &db,
                    crate::auth::audit_log::event::RATE_LIMIT_REJECTED,
                    crate::auth::audit_log::Subjects::anonymous(),
                    None,
                    None,
                    crate::auth::audit_log::Change::none(),
                    serde_json::json!({ "bucket": bucket }),
                )
                .await;
            });
        }

        rate_limit_exceeded(error)
    }
}

/// See the doc comment on its `.layer(...)` call site in `with_response_layers`
/// above. Only rewrites a response that (a) has one of the three status codes
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
