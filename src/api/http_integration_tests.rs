//! Real HTTP-level tests: bind the actual `router()` to a real loopback
//! port via `axum::serve`, then hit it with a real `reqwest` client. Every
//! other test in this crate calls a handler function directly (see the
//! doc comment on `test_support` above) -- that's the right default for
//! testing handler logic, but it can never exercise anything that only
//! runs when a request actually passes through the real `Router` and its
//! middleware stack: CORS, the panic-catching layer, content-type
//! extraction, and so on. The CORS credentials bug this suite regression-
//! tests is exactly that class of bug -- it was invisible to every
//! direct-call test in this crate and was only ever caught by hand in a
//! real browser.

use std::net::SocketAddr;

use super::test_support::empty_state;

async fn spawn_test_server() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binding to an OS-assigned loopback port should never fail");

    let addr = listener
        .local_addr()
        .expect("a bound listener always has a local address");

    let app = super::router(empty_state());

    tokio::spawn(async move {
        // Matches main.rs's real serve call: the auth rate limiter (see
        // `router()`'s `auth_rate_limit` construction) is keyed off
        // `ConnectInfo<SocketAddr>`, which `into_make_service` alone never
        // populates -- without this, every request to a rate-limited
        // route would hit `GovernorError::UnableToExtractKey` instead of
        // being counted at all.
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("test server should not fail to serve");
    });

    addr
}

#[tokio::test]
async fn health_endpoint_responds_over_a_real_http_connection() {
    let addr = spawn_test_server().await;

    let response = reqwest::get(format!("http://{addr}/health"))
        .await
        .expect("request should reach the real server");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
}

/// Regression test for the real CORS bug found and fixed alongside the
/// v1.1.1 hygiene pass: a credentialed cross-origin response is invisible
/// to a real browser unless the server echoes back both
/// `Access-Control-Allow-Credentials: true` and the specific requesting
/// origin (never `*`, which credentialed CORS forbids). This is the
/// mechanism `useSessionPost`/`useSessionAction`'s `credentials: "include"`
/// depends on -- a direct handler call can never exercise it, since
/// `CorsLayer` only runs for requests that go through the real `Router`.
#[tokio::test]
async fn credentialed_cross_origin_request_gets_the_headers_a_browser_requires() {
    let addr = spawn_test_server().await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("http://{addr}/health"))
        .header("Origin", "http://localhost:3000")
        .send()
        .await
        .expect("request should reach the real server");

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let allow_credentials = response
        .headers()
        .get("access-control-allow-credentials")
        .and_then(|v| v.to_str().ok());
    assert_eq!(allow_credentials, Some("true"));

    let allow_origin = response
        .headers()
        .get("access-control-allow-origin")
        .and_then(|v| v.to_str().ok());
    assert_eq!(allow_origin, Some("http://localhost:3000"));
}

/// The mirror image of the test above: an origin that was never on the
/// allow-list must not get `Access-Control-Allow-Origin` echoed back --
/// that's the specific header that tells a real browser it's safe to hand
/// the response to the page's own script, and it's conditional on the
/// origin actually matching. `Access-Control-Allow-Credentials` is a
/// static per-layer setting rather than conditional on origin match, so
/// it's present either way -- harmless on its own, since a browser also
/// requires a matching `Allow-Origin` before it exposes anything.
#[tokio::test]
async fn cross_origin_request_from_an_unlisted_origin_gets_no_allow_origin_header() {
    let addr = spawn_test_server().await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("http://{addr}/health"))
        .header("Origin", "http://evil.example")
        .send()
        .await
        .expect("request should reach the real server");

    assert!(response
        .headers()
        .get("access-control-allow-origin")
        .is_none());
}

/// Regression test for the error-body-shape fix: malformed JSON used to
/// reject with a plain-text body ("Failed to parse the request body as
/// JSON: ...") before any handler ever ran, unlike every other error path
/// in this API (`{error, message}` JSON). Confirmed live against the real
/// router, not a direct handler call, since `Json<T>`'s own rejection
/// only happens at actual request-extraction time.
#[tokio::test]
async fn malformed_json_body_gets_the_standard_error_shape() {
    let addr = spawn_test_server().await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{addr}/correct"))
        .header("Content-Type", "application/json")
        .body("{not valid json")
        .send()
        .await
        .expect("request should reach the real server");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("application/json"));

    let body: serde_json::Value = response
        .json()
        .await
        .expect("error body should itself be valid JSON");

    assert_eq!(body["error"], "invalid_request_body");
    assert!(body["message"].as_str().is_some_and(|m| !m.is_empty()));
}

/// The other confirmed symptom of the same gap: posting JSON without the
/// `Content-Type: application/json` header used to reject with a
/// plain-text 415 body instead of the standard JSON error shape.
#[tokio::test]
async fn wrong_content_type_gets_the_standard_error_shape() {
    let addr = spawn_test_server().await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{addr}/correct"))
        .header("Content-Type", "text/plain")
        .body("{}")
        .send()
        .await
        .expect("request should reach the real server");

    assert_eq!(
        response.status(),
        reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
    );

    let body: serde_json::Value = response
        .json()
        .await
        .expect("error body should itself be valid JSON");

    assert_eq!(body["error"], "unsupported_media_type");
}

/// A handler's own legitimately-JSON error response (not an extraction
/// rejection) must pass through this layer completely untouched.
#[tokio::test]
async fn a_handlers_own_json_error_response_is_not_rewritten() {
    let addr = spawn_test_server().await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("http://{addr}/correct"))
        .json(&serde_json::json!({
            "session_id": "missing",
            "file_name": "units.csv",
            "unit_number": "A01",
            "field": "width",
            "value": "10"
        }))
        .send()
        .await
        .expect("request should reach the real server");

    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

    let body: serde_json::Value = response
        .json()
        .await
        .expect("error body should itself be valid JSON");

    assert_eq!(body["error"], "session_not_found");
}

/// The one handler whose input (`multipart/form-data`) is awkward to
/// construct by calling the function directly -- `upload_tests.rs`
/// already covers this via axum's own `Multipart::from_request`, but
/// driving it through a real HTTP client too proves the route is wired
/// correctly end to end, not just that the handler function works in
/// isolation.
#[tokio::test]
async fn upload_endpoint_accepts_a_real_multipart_request_over_http() {
    let addr = spawn_test_server().await;
    let client = reqwest::Client::new();

    let file_part = reqwest::multipart::Part::text("Number,UnitGroup\nA01,10x10 Climate\n")
        .file_name("units.csv")
        .mime_str("text/csv")
        .expect("text/csv is a valid mime type");

    let form = reqwest::multipart::Form::new().part("files", file_part);

    let response = client
        .post(format!("http://{addr}/upload"))
        .multipart(form)
        .send()
        .await
        .expect("request should reach the real server");

    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let body: serde_json::Value = response
        .json()
        .await
        .expect("upload response should be valid JSON");

    assert!(body.get("session_id").is_some());
}

/// Regression coverage for the size limit configured on the router
/// (`DefaultBodyLimit::max(100 * 1024 * 1024)`, see its `.layer(...)`
/// call in `router()` above) -- structurally untestable from
/// `upload_tests.rs`, since every test there deliberately calls
/// `Multipart::from_request` and the `upload` handler directly (see
/// that file's own doc comment), bypassing the Router's `.layer(...)`
/// stack -- and therefore `DefaultBodyLimit` -- entirely. This lives
/// here instead, alongside every other test that needs the real
/// `Router`, for the same reason
/// `upload_endpoint_accepts_a_real_multipart_request_over_http` above
/// does.
///
/// Deliberately targets `/correct` (a `Json<T>` handler), not `/upload`:
/// `Multipart`'s own extractor rejects a non-multipart `Content-Type`
/// immediately, without ever reading the body far enough to trip
/// `DefaultBodyLimit` at all -- confirmed empirically (that variant of
/// this test failed with a plain 400, not 413). `Json<T>` extraction
/// buffers the whole body via `Bytes` first, which is exactly where
/// `DefaultBodyLimit`'s wrapped body stream errors out once the
/// cumulative read exceeds the limit, before JSON parsing ever starts --
/// so the oversized body never needs to be valid JSON (or even valid
/// UTF-8) to trip this.
#[tokio::test]
async fn oversized_request_body_is_rejected_with_the_standard_error_shape() {
    let addr = spawn_test_server().await;
    let client = reqwest::Client::new();

    // One byte over the configured limit -- enough to trip it, not an
    // arbitrarily larger amount.
    let oversized_body = vec![0u8; 100 * 1024 * 1024 + 1];

    let response = client
        .post(format!("http://{addr}/correct"))
        .header("Content-Type", "application/json")
        .body(oversized_body)
        .send()
        .await
        .expect("request should reach the real server");

    assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("application/json"));

    let body: serde_json::Value = response
        .json()
        .await
        .expect("error body should itself be valid JSON");

    assert_eq!(body["error"], "payload_too_large");
}

/// Consolidated sweep across several DIFFERENT error-producing endpoints
/// and failure modes, rather than one more test of a single handler --
/// the point is confirming this API's `{error, message}` `ApiErrorBody`
/// shape is the real, observable contract across the whole surface, not
/// just wherever a prior audit happened to look. Combines the
/// already-individually-tested failure modes above (malformed JSON,
/// wrong Content-Type -- exercised here against `/discover` instead of
/// `/correct`, proving the rewrite middleware applies router-wide, not
/// to one hardcoded route) with a genuinely unknown `session_id` on
/// three further endpoints, since `session_not_found` is a handler's own
/// legitimately-JSON error (see
/// `a_handlers_own_json_error_response_is_not_rewritten` above) --
/// proving that's true everywhere a session lookup can fail, not just
/// for `/correct`.
#[tokio::test]
async fn a_sweep_of_different_error_endpoints_all_share_the_standard_error_shape() {
    let addr = spawn_test_server().await;
    let client = reqwest::Client::new();

    async fn assert_standard_error_shape(
        response: reqwest::Response,
        expected_status: reqwest::StatusCode,
        expected_error: &str,
    ) {
        assert_eq!(response.status(), expected_status);

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(content_type.starts_with("application/json"));

        let body: serde_json::Value = response
            .json()
            .await
            .expect("error body should itself be valid JSON");

        assert_eq!(body["error"], expected_error);
        assert!(body["message"].as_str().is_some_and(|m| !m.is_empty()));
    }

    // A genuinely unknown session id, on three different endpoints.
    for path in ["/analyze", "/export", "/validate"] {
        let response = client
            .post(format!("http://{addr}{path}"))
            .json(&serde_json::json!({ "session_id": "missing" }))
            .send()
            .await
            .expect("request should reach the real server");

        assert_standard_error_shape(
            response,
            reqwest::StatusCode::NOT_FOUND,
            "session_not_found",
        )
        .await;
    }

    // Malformed JSON.
    let malformed_json_response = client
        .post(format!("http://{addr}/discover"))
        .header("Content-Type", "application/json")
        .body("{not valid json")
        .send()
        .await
        .expect("request should reach the real server");

    assert_standard_error_shape(
        malformed_json_response,
        reqwest::StatusCode::BAD_REQUEST,
        "invalid_request_body",
    )
    .await;

    // Wrong Content-Type.
    let wrong_content_type_response = client
        .post(format!("http://{addr}/discover"))
        .header("Content-Type", "text/plain")
        .body("{}")
        .send()
        .await
        .expect("request should reach the real server");

    assert_standard_error_shape(
        wrong_content_type_response,
        reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE,
        "unsupported_media_type",
    )
    .await;
}

/// Regression coverage for the auth-endpoint rate limiter (Phase I item 2
/// -- see `router()`'s `auth_rate_limit` construction). This specifically
/// needs the real HTTP harness, not a direct handler call: the limiter is
/// keyed off `ConnectInfo<SocketAddr>`, which only exists once a request
/// has gone through a real accepted TCP connection -- see
/// `spawn_test_server`'s own comment on why it now uses
/// `into_make_service_with_connect_info`.
///
/// Deliberately hits `/auth/login/begin` without expecting it to succeed:
/// `empty_state`'s pool is unreachable, so every request that gets past
/// the limiter fails downstream with a 500 once it tries the database
/// lookup. That is exactly what makes this a clean test of the *limiter*
/// alone -- the configured burst size (10) worth of requests must all be
/// admitted (whatever they fail with afterwards is irrelevant here), and
/// the one response that must be exactly 429 is the 11th, proving the
/// governor layer rejected it before the handler -- and its doomed DB
/// query -- ever ran.
#[tokio::test]
async fn the_auth_rate_limit_rejects_a_burst_past_its_configured_size() {
    let addr = spawn_test_server().await;
    let client = reqwest::Client::new();

    async fn login_begin_attempt(client: &reqwest::Client, addr: SocketAddr) -> reqwest::Response {
        client
            .post(format!("http://{addr}/auth/login/begin"))
            .json(&serde_json::json!({ "email": "ratelimit-probe@example.com" }))
            .send()
            .await
            .expect("request should reach the real server")
    }

    for attempt in 1..=10 {
        let response = login_begin_attempt(&client, addr).await;
        assert_ne!(
            response.status(),
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "attempt {attempt} of the configured burst size must not be rate-limited"
        );
    }

    // Past the burst -- rejected by the limiter itself, and with this
    // project's standard error shape rather than tower_governor's own
    // plain-text default (see `rate_limit_exceeded` in `router`'s file).
    let response = login_begin_attempt(&client, addr).await;
    assert_eq!(response.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);

    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(content_type.starts_with("application/json"));

    let body: serde_json::Value = response
        .json()
        .await
        .expect("the rate-limit rejection body should itself be valid JSON");

    assert_eq!(body["error"], "rate_limited");
    assert!(body["message"].as_str().is_some_and(|m| !m.is_empty()));
}

/// The mirror image: `/auth/invites` has its own, separate bucket
/// (`invite_rate_limit`) rather than sharing the one above -- confirmed
/// by exhausting the shared auth bucket first and showing invites is
/// unaffected, which a passing burst-size test for invites alone could
/// not distinguish from "there is only one bucket and it happens to be
/// generous enough."
#[tokio::test]
async fn the_invite_rate_limit_is_independent_of_the_auth_rate_limit() {
    let addr = spawn_test_server().await;
    let client = reqwest::Client::new();

    async fn login_begin_attempt(client: &reqwest::Client, addr: SocketAddr) -> reqwest::Response {
        client
            .post(format!("http://{addr}/auth/login/begin"))
            .json(&serde_json::json!({ "email": "ratelimit-probe@example.com" }))
            .send()
            .await
            .expect("request should reach the real server")
    }

    // Exhaust the auth bucket's burst allowance and confirm it is
    // actually exhausted (the 11th request here is the same assertion as
    // the test above, kept as a precondition rather than assumed).
    for _ in 1..=10 {
        login_begin_attempt(&client, addr).await;
    }
    let exhausted = login_begin_attempt(&client, addr).await;
    assert_eq!(exhausted.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);

    // `/auth/invites` requires an authenticated admin, so this will 401 --
    // which is the point: a 401 (reached the handler) proves the request
    // was never counted against the exhausted auth bucket, unlike a 429
    // (rejected by a shared one) which would prove the opposite.
    let invite_response = client
        .post(format!("http://{addr}/auth/invites"))
        .json(&serde_json::json!({
            "email": "someone@example.com",
            "first_name": "Ada",
            "last_name": "Lovelace",
            "company": "quikstor",
        }))
        .send()
        .await
        .expect("request should reach the real server");

    assert_ne!(
        invite_response.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "the invite endpoint must not share the auth endpoints' rate-limit bucket"
    );
}
