//! A WARN-level log line for an operation that took too long, so a
//! slowdown shows up in the logs on its own instead of waiting for
//! someone to notice the screen taking seconds.
//!
//! Dedup already logged `check_ms` on every run, which is how the ~9 s
//! analysis was diagnosed afterwards; nothing flagged it at the time. The
//! usual causes, in the order they have actually bitten: a debug build
//! running an allocation-heavy loop (the dev server is one), a pass that
//! is quadratic in the number of tenants without pruning, and an `await`
//! inside a loop over N external calls.

use std::time::Duration;

/// Slower than this is worth a warning. The dedup analysis is ~0.1 s on a
/// real 759-row facility (debug build), so 2 s is ~20x normal.
pub(crate) const SLOW_OPERATION_THRESHOLD: Duration = Duration::from_secs(2);

pub(crate) fn is_slow(elapsed: Duration) -> bool {
    elapsed >= SLOW_OPERATION_THRESHOLD
}

/// Logs a warning naming `operation` when `elapsed` is over the threshold;
/// does nothing otherwise. Returns whether it warned.
pub(crate) fn warn_if_slow(operation: &'static str, elapsed: Duration) -> bool {
    if !is_slow(elapsed) {
        return false;
    }

    tracing::warn!(
        operation,
        elapsed_ms = elapsed.as_millis() as u64,
        threshold_ms = SLOW_OPERATION_THRESHOLD.as_millis() as u64,
        "Operation exceeded the slow-operation threshold -- check for a debug build running a heavy loop, an unpruned pairwise pass, or sequential calls to an external service"
    );
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_operation_under_the_threshold_is_not_slow() {
        assert!(!is_slow(Duration::from_millis(1_999)));
        assert!(!warn_if_slow("test", Duration::from_millis(91)));
    }

    #[test]
    fn an_operation_at_or_over_the_threshold_is_slow_and_warns() {
        assert!(is_slow(Duration::from_secs(2)));
        assert!(warn_if_slow("test", Duration::from_millis(9_162)));
    }
}
