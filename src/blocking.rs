//! The one primitive for running synchronous CPU-bound work on tokio's
//! blocking pool without losing the caller's tracing span.
//!
//! Lives at the crate root (not under `api`) because the code that needs
//! it is not only HTTP handlers -- the tool-run encryption in
//! `client_ops` does too, and a domain module should not import from the
//! HTTP layer. The handler-facing wrappers (panic -> standard 500 response,
//! session-store closures) stay in `api::blocking`, which builds on this.

use tokio::task::JoinError;
use tracing::Span;

/// Runs `work` on the blocking pool inside the caller's current span and
/// hands back the raw join result.
///
/// `spawn_blocking` runs its closure on a different thread with no
/// current span, so every `tracing::` line emitted inside it would
/// silently lose its `request_id`. The span is captured before the hop
/// and re-entered inside.
pub(crate) async fn spawn_blocking_in_span<T, F>(work: F) -> Result<T, JoinError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let span = Span::current();
    tokio::task::spawn_blocking(move || span.in_scope(work)).await
}
