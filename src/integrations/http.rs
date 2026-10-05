//! One outbound-HTTP policy for every third-party integration (Process
//! Street, Dropbox, ClickUp): bounded timeouts, a small retry helper for
//! transient failures, and a log-safe body truncator.
//!
//! **Why this exists.** `reqwest::Client::new()` has no timeout of any
//! kind, so a hung upstream pinned the calling handler indefinitely -- and
//! for Dropbox, while holding the token mutex, stalled every other
//! Dropbox call in the process. A 429 or a transient 5xx surfaced as a
//! hard error with no second attempt, and full upstream response bodies
//! (which can carry customer data) were written to the error log.
//!
//! **What retries, and what does not.** [`send_with_retry`] retries only
//! failures that are transient by nature: a connect error, a timeout, and
//! HTTP 429/500/502/503/504. It honours a numeric `Retry-After`, but a
//! `Retry-After` longer than [`MAX_RETRY_AFTER`] is NOT waited out -- the
//! response is returned as-is so a user-facing request fails fast instead
//! of hanging for a minute. Callers must only pass requests that are safe
//! to repeat (every Process Street and ClickUp call here is a GET; Dropbox
//! reads and the idempotent `create_folder_v2` qualify; an overwrite
//! upload deliberately does not use it).

use std::borrow::Cow;
use std::future::Future;
use std::time::Duration;

use futures::stream::{self, StreamExt};

use reqwest::{RequestBuilder, Response, StatusCode};

/// How long to wait for a TCP+TLS connection before giving up.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Overall per-request ceiling (connect + send + full response body).
/// Individual calls that legitimately move large payloads (Dropbox
/// download/upload) override it per request.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The longest `Retry-After` this helper will sit through. Anything
/// longer is returned to the caller unretried.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(10);

/// A `reqwest` client builder with the shared connect and request
/// timeouts already applied. Callers add anything integration-specific
/// (a tighter timeout, headers) and call `.build()`.
pub fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
}

#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Total tries including the first. `1` means never retry.
    pub max_attempts: u32,
    /// First backoff; doubles each retry, with up to 25% added jitter.
    pub base_delay: Duration,
    /// Ceiling on any single backoff (does not apply to `Retry-After`,
    /// which is bounded by [`MAX_RETRY_AFTER`] instead).
    pub max_delay: Duration,
}

impl RetryPolicy {
    /// Three tries with 0.5 s / 1 s backoff. For ordinary reads.
    pub const STANDARD: Self = Self {
        max_attempts: 3,
        base_delay: Duration::from_millis(500),
        max_delay: Duration::from_secs(8),
    };

    /// Two tries with a short backoff. For work done while holding a lock
    /// or a user is plainly waiting (the Dropbox token refresh).
    pub const QUICK: Self = Self {
        max_attempts: 2,
        base_delay: Duration::from_millis(250),
        max_delay: Duration::from_secs(2),
    };
}

fn is_retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504)
}

fn is_retryable_error(err: &reqwest::Error) -> bool {
    err.is_timeout() || err.is_connect()
}

/// Backoff for the `attempt`-th retry (0-based): `base * 2^attempt`,
/// capped at `max_delay`, plus up to 25% jitter so a burst of callers
/// that failed together do not retry in lockstep.
fn backoff(policy: &RetryPolicy, attempt: u32) -> Duration {
    let exponential = policy
        .base_delay
        .saturating_mul(2u32.saturating_pow(attempt))
        .min(policy.max_delay);

    let mut byte = [0u8; 1];
    // Jitter only needs to be uncorrelated, not secret; if the OS RNG is
    // somehow unavailable, no jitter is a fine degradation.
    let jitter_fraction = match getrandom::fill(&mut byte) {
        Ok(()) => f64::from(byte[0]) / 255.0 * 0.25,
        Err(_) => 0.0,
    };

    exponential + exponential.mul_f64(jitter_fraction)
}

/// A numeric `Retry-After` (seconds). The HTTP-date form is ignored --
/// none of the integrations here send it.
fn retry_after(response: &Response) -> Option<Duration> {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Sends the request `build` produces, retrying transient failures per
/// `policy`. `build` is called once per attempt because a
/// `RequestBuilder` is consumed by sending.
///
/// Returns the final [`Response`] even when it is still a 429/5xx after
/// the last attempt (so the caller's existing status handling and error
/// mapping run unchanged), and `Err` only when every attempt failed
/// before getting a response.
pub async fn send_with_retry<F>(policy: RetryPolicy, build: F) -> Result<Response, reqwest::Error>
where
    F: Fn() -> RequestBuilder,
{
    let attempts = policy.max_attempts.max(1);
    let mut attempt = 0u32;

    loop {
        attempt += 1;
        let is_last = attempt >= attempts;

        match build().send().await {
            Ok(response) => {
                if is_last || !is_retryable_status(response.status()) {
                    return Ok(response);
                }

                let wait = match retry_after(&response) {
                    Some(requested) if requested > MAX_RETRY_AFTER => return Ok(response),
                    Some(requested) => requested,
                    None => backoff(&policy, attempt - 1),
                };

                tracing::warn!(
                    status = response.status().as_u16(),
                    attempt,
                    wait_ms = wait.as_millis() as u64,
                    "transient upstream status, retrying"
                );
                tokio::time::sleep(wait).await;
            }
            Err(err) => {
                if is_last || !is_retryable_error(&err) {
                    return Err(err);
                }

                let wait = backoff(&policy, attempt - 1);
                tracing::warn!(
                    timeout = err.is_timeout(),
                    attempt,
                    wait_ms = wait.as_millis() as u64,
                    "transient upstream error, retrying"
                );
                tokio::time::sleep(wait).await;
            }
        }
    }
}

/// The most calls to ONE third-party API this process makes at the same
/// time from one fan-out. Process Street allows ~2,500 requests an hour and
/// each Re-sync / import preview can cite dozens of runs; an unbounded
/// `join_all` fires them all at once, which risks a 429 and (with retries)
/// makes it worse. Six keeps the wall-clock win of running them together
/// without a burst.
pub const MAX_CONCURRENT_UPSTREAM_CALLS: usize = 6;

/// Runs `futures` with at most [`MAX_CONCURRENT_UPSTREAM_CALLS`] in flight
/// and returns their outputs **in input order** -- a drop-in replacement
/// for `join_all` wherever the number of futures is data-driven (one per
/// run, per facility, per file) and each one is a call to an upstream API.
pub fn join_all_bounded<I, Fut>(futures: I) -> impl Future<Output = Vec<Fut::Output>>
where
    I: IntoIterator<Item = Fut>,
    Fut: Future,
{
    // Collected up front and returned as a plain (non-) future,
    // exactly as  does. An  would keep its argument --
    // the lazy iterator and the closures that build the futures -- in its
    // state until it completes, and that trips the compiler's
    // higher-ranked-lifetime checks in axum handlers ("implementation of
    //  is not general enough").
    let futures: Vec<Fut> = futures.into_iter().collect();

    stream::iter(futures)
        .buffered(MAX_CONCURRENT_UPSTREAM_CALLS)
        .collect()
}

/// Longest upstream response body written to a log line. Upstream error
/// bodies can echo customer data back, so logs get a bounded prefix; the
/// full body still travels in the returned error for callers that need it.
pub const MAX_LOGGED_BODY_BYTES: usize = 512;

/// `body` cut to at most `max_bytes` (on a char boundary), with a marker
/// saying how much was dropped. Borrows when no cutting is needed.
pub fn truncate_for_log(body: &str, max_bytes: usize) -> Cow<'_, str> {
    if body.len() <= max_bytes {
        return Cow::Borrowed(body);
    }

    let mut end = max_bytes;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }

    Cow::Owned(format!(
        "{}... [truncated, {} more bytes]",
        &body[..end],
        body.len() - end
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use axum::extract::State;
    use axum::http::{HeaderMap, HeaderValue};
    use axum::routing::get;
    use axum::Router;

    use super::*;

    /// Exactly one attempt: nothing retries.
    const NONE: RetryPolicy = RetryPolicy {
        max_attempts: 1,
        base_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
    };

    /// A tiny policy so the tests do not sit through real backoffs.
    const FAST: RetryPolicy = RetryPolicy {
        max_attempts: 3,
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
    };

    #[derive(Clone)]
    struct Step {
        status: u16,
        retry_after: Option<&'static str>,
        delay: Duration,
    }

    fn step(status: u16) -> Step {
        Step {
            status,
            retry_after: None,
            delay: Duration::ZERO,
        }
    }

    #[derive(Clone)]
    struct Script {
        steps: Arc<Vec<Step>>,
        calls: Arc<AtomicUsize>,
    }

    /// A loopback server that answers request N with `steps[N]` (the last
    /// step repeats). Returns its URL and the call counter.
    async fn spawn_server(steps: Vec<Step>) -> (String, Arc<AtomicUsize>) {
        let script = Script {
            steps: Arc::new(steps),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let calls = script.calls.clone();

        async fn handler(State(script): State<Script>) -> (StatusCode, HeaderMap, &'static str) {
            let n = script.calls.fetch_add(1, Ordering::SeqCst);
            let step = script.steps[n.min(script.steps.len() - 1)].clone();
            tokio::time::sleep(step.delay).await;

            let mut headers = HeaderMap::new();
            if let Some(value) = step.retry_after {
                headers.insert("retry-after", HeaderValue::from_static(value));
            }
            (StatusCode::from_u16(step.status).unwrap(), headers, "body")
        }

        let app = Router::new().route("/", get(handler)).with_state(script);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (url, calls)
    }

    fn client() -> reqwest::Client {
        reqwest::Client::new()
    }

    #[tokio::test]
    async fn retries_a_transient_5xx_and_returns_the_eventual_success() {
        let (url, calls) = spawn_server(vec![step(503), step(502), step(200)]).await;
        let http = client();

        let response = send_with_retry(FAST, || http.get(&url)).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn returns_the_last_response_when_attempts_run_out() {
        let (url, calls) = spawn_server(vec![step(503)]).await;
        let http = client();

        let response = send_with_retry(FAST, || http.get(&url)).await.unwrap();

        assert_eq!(
            response.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "the caller's own status handling must still see the final 503"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn does_not_retry_a_permanent_client_error() {
        let (url, calls) = spawn_server(vec![step(404)]).await;
        let http = client();

        let response = send_with_retry(FAST, || http.get(&url)).await.unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn never_retries_with_a_single_attempt_policy() {
        let (url, calls) = spawn_server(vec![step(503)]).await;
        let http = client();

        let response = send_with_retry(NONE, || http.get(&url)).await.unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn honours_a_short_retry_after_on_429() {
        let (url, calls) = spawn_server(vec![
            Step {
                status: 429,
                retry_after: Some("0"),
                delay: Duration::ZERO,
            },
            step(200),
        ])
        .await;
        let http = client();

        let response = send_with_retry(FAST, || http.get(&url)).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn fails_fast_instead_of_waiting_out_a_long_retry_after() {
        let (url, calls) = spawn_server(vec![Step {
            status: 429,
            retry_after: Some("3600"),
            delay: Duration::ZERO,
        }])
        .await;
        let http = client();

        let started = std::time::Instant::now();
        let response = send_with_retry(FAST, || http.get(&url)).await.unwrap();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a Retry-After beyond MAX_RETRY_AFTER must not be retried"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "and must not have slept through it"
        );
    }

    #[tokio::test]
    async fn retries_a_timeout_then_gives_up_with_a_timeout_error() {
        let (url, calls) = spawn_server(vec![Step {
            status: 200,
            retry_after: None,
            delay: Duration::from_secs(5),
        }])
        .await;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap();
        let policy = RetryPolicy {
            max_attempts: 2,
            ..FAST
        };

        let err = send_with_retry(policy, || http.get(&url))
            .await
            .unwrap_err();

        assert!(err.is_timeout());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retries_a_refused_connection_then_gives_up_with_a_connect_error() {
        // Bind to learn a free port, then drop the listener so nothing
        // is listening there.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let http = client();

        let err = send_with_retry(FAST, || http.get(&url)).await.unwrap_err();

        assert!(err.is_connect());
    }

    #[tokio::test]
    async fn bounded_fan_out_never_exceeds_the_limit_and_keeps_input_order() {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let work = (0..40usize).map(|n| {
            let (in_flight, peak) = (in_flight.clone(), peak.clone());
            async move {
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                // Later items finish sooner, so an unordered collector would
                // return them out of order.
                tokio::time::sleep(Duration::from_millis(((40 - n) % 7) as u64 + 1)).await;
                in_flight.fetch_sub(1, Ordering::SeqCst);
                n
            }
        });

        let results = join_all_bounded(work).await;

        assert_eq!(
            results,
            (0..40usize).collect::<Vec<_>>(),
            "input order kept"
        );
        assert!(
            peak.load(Ordering::SeqCst) <= MAX_CONCURRENT_UPSTREAM_CALLS,
            "at most {MAX_CONCURRENT_UPSTREAM_CALLS} in flight, saw {}",
            peak.load(Ordering::SeqCst)
        );
        assert!(
            peak.load(Ordering::SeqCst) > 1,
            "and it must actually run them concurrently, not one by one"
        );
    }

    #[test]
    fn backoff_grows_and_respects_its_ceiling() {
        let policy = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
        };

        let first = backoff(&policy, 0);
        let second = backoff(&policy, 1);
        let huge = backoff(&policy, 30);

        assert!(first >= Duration::from_millis(100) && first <= Duration::from_millis(125));
        assert!(second >= Duration::from_millis(200) && second <= Duration::from_millis(250));
        assert!(huge >= Duration::from_millis(500) && huge <= Duration::from_millis(625));
    }

    #[test]
    fn truncation_leaves_short_bodies_alone_and_marks_cut_ones() {
        assert_eq!(truncate_for_log("short", 512), "short");

        let long = "x".repeat(600);
        let cut = truncate_for_log(&long, 512);
        assert!(cut.starts_with(&"x".repeat(512)));
        assert!(cut.ends_with("[truncated, 88 more bytes]"));
    }

    #[test]
    fn truncation_never_splits_a_multibyte_character() {
        // 'é' is two bytes; a cut at an odd byte index would panic if the
        // helper sliced blindly.
        let body = "é".repeat(400);
        let cut = truncate_for_log(&body, 511);
        assert!(cut.starts_with("é"));
        assert!(cut.contains("[truncated"));
    }
}
