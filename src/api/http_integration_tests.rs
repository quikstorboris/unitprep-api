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
        axum::serve(listener, app)
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
