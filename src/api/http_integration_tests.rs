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
