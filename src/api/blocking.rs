//! Running CPU-heavy synchronous work off the async worker threads.
//!
//! **Why this exists.** Parsing an uploaded spreadsheet, building a dedup
//! report or generating an XLSX/ZIP export is plain CPU-bound code. Called
//! directly from an `async fn` handler it occupies one of tokio's few
//! worker threads for its whole duration (hundreds of milliseconds to
//! seconds for a big facility), and every unrelated request scheduled on
//! that worker -- including the auth lookup every authenticated request
//! starts with -- waits behind it. Until this module the only
//! `spawn_blocking` calls in the crate were the two audit-log PDF exports.
//!
//! [`run_blocking`] is the one place that does it, so every call site gets
//! the same two properties the hand-rolled version did not:
//!
//! - **The request's tracing span follows the work.** `spawn_blocking`
//!   runs the closure on a different thread with no current span, so every
//!   `tracing::` line emitted inside it (the parsers' "Skipping file",
//!   the session service's "Creating session") would silently lose its
//!   `request_id`. The span is captured before the hop and re-entered
//!   inside.
//! - **A panic becomes the project's own 500**, logged, exactly like a
//!   panic in the handler body would (`CatchPanicLayer`), instead of an
//!   unhandled `JoinError`.

use std::sync::Arc;

use axum::response::Response;
use uuid::Uuid;

use unitprep_core::session::HasSessionMetadata;
use unitprep_core::session_store::{SessionStore, SessionStoreExt};

use super::internal_error;

/// The lower-level form (defined in `crate::blocking` so non-HTTP code can
/// use it too): runs `work` on the blocking pool inside the caller's
/// current span and hands back the raw join result. Use this where a panic
/// should be handled some way other than a 500 response (a per-file
/// failure inside a larger scan, say).
pub(crate) use crate::blocking::spawn_blocking_in_span;

/// Runs `work` on the blocking pool. If it panics (or the runtime is
/// shutting down), logs it and returns the standard `internal_error`
/// response, naming `operation` in both.
///
/// `work` must own everything it touches (`'static`): move the uploaded
/// files, vendor lists and so on in, and return anything the handler still
/// needs afterwards.
pub(crate) async fn run_blocking<T, F>(operation: &'static str, work: F) -> Result<T, Response>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    spawn_blocking_in_span(work).await.map_err(|err| {
        tracing::error!(
            operation,
            panicked = err.is_panic(),
            error = %err,
            "blocking task failed"
        );
        internal_error(operation)
    })
}

/// `SessionStoreExt::with_owned_session_mut` on the blocking pool.
///
/// Many handlers do their real work -- re-deriving a discovery, validating
/// every document, mapping a format -- inside the closure they hand the
/// session store, i.e. under the session's write lock. Running that on an
/// async worker blocks the worker for the whole computation; running it
/// here blocks only a blocking-pool thread. The lock semantics are
/// unchanged: it is taken and released inside the one blocking call.
///
/// `op` must own what it captures (`'static`). Handlers typically wrap
/// their parsed `request` in an `Arc` and give the closure a clone, so the
/// closure body can keep reading `request.field` exactly as before.
/// `Ok(None)` is the store's own "no such session (or not yours)".
pub(crate) async fn with_owned_session_mut_blocking<S, R, F>(
    operation: &'static str,
    store: &Arc<dyn SessionStore<S>>,
    session_id: &str,
    owner_id: Uuid,
    op: F,
) -> Result<Option<R>, Response>
where
    S: HasSessionMetadata + 'static,
    F: FnOnce(&mut S) -> R + Send + 'static,
    R: Send + 'static,
{
    let store = Arc::clone(store);
    let session_id = session_id.to_string();

    run_blocking(operation, move || {
        store.with_owned_session_mut(&session_id, owner_id, op)
    })
    .await
}

/// Read-only counterpart of [`with_owned_session_mut_blocking`].
pub(crate) async fn with_owned_session_blocking<S, R, F>(
    operation: &'static str,
    store: &Arc<dyn SessionStore<S>>,
    session_id: &str,
    owner_id: Uuid,
    op: F,
) -> Result<Option<R>, Response>
where
    S: HasSessionMetadata + 'static,
    F: FnOnce(&S) -> R + Send + 'static,
    R: Send + 'static,
{
    let store = Arc::clone(store);
    let session_id = session_id.to_string();

    run_blocking(operation, move || {
        store.with_owned_session(&session_id, owner_id, op)
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use axum::http::StatusCode;
    use tracing::Span;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    /// The property that matters: while the blocking work runs, the async
    /// runtime stays free to run other tasks. On a SINGLE-thread runtime a
    /// ticker task can only advance if nothing is hogging that thread --
    /// run the same sleep inline and the ticker count would be zero.
    #[tokio::test(flavor = "current_thread")]
    async fn the_runtime_keeps_serving_other_tasks_while_blocking_work_runs() {
        let ticks = Arc::new(AtomicUsize::new(0));
        let ticker = {
            let ticks = ticks.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    ticks.fetch_add(1, Ordering::SeqCst);
                }
            })
        };

        let answer = run_blocking("test", || {
            std::thread::sleep(Duration::from_millis(300));
            42
        })
        .await
        .unwrap();

        ticker.abort();
        assert_eq!(answer, 42);
        assert!(
            ticks.load(Ordering::SeqCst) >= 10,
            "the runtime must have kept running other tasks during the 300 ms of blocking work (got {} ticks)",
            ticks.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn a_panic_in_the_work_becomes_the_standard_500() {
        let response = run_blocking("explode", || -> u32 { panic!("boom") })
            .await
            .unwrap_err();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "internal_error");
        assert!(json["message"].as_str().unwrap().contains("explode"));
    }

    #[tokio::test]
    async fn the_callers_tracing_span_follows_the_work_onto_the_blocking_thread() {
        // A registry subscriber assigns real span ids (with no subscriber
        // installed, `Span::current()` is always disabled and comparing ids
        // would prove nothing).
        let subscriber = tracing_subscriber::registry().with(tracing_subscriber::fmt::layer());
        let _guard = tracing::subscriber::set_default(subscriber);

        let span = tracing::info_span!("http_request", request_id = "abc");
        let outer_id = span.id().expect("a registry subscriber assigns an id");

        // The blocking thread has no thread-local subscriber of its own
        // (in production the global default covers it), so hand it this
        // test's dispatcher. What is under test is that the span was
        // ENTERED on that thread, which the registry records per thread.
        let dispatch = tracing::dispatcher::get_default(|d| d.clone());

        let inner_id = {
            let _entered = span.enter();
            run_blocking("span", move || {
                tracing::dispatcher::with_default(&dispatch, || Span::current().id())
            })
            .await
            .unwrap()
        };

        assert_eq!(
            inner_id,
            Some(outer_id),
            "log lines inside the closure must keep the request's span (and its request_id)"
        );
    }
}
