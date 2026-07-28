use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::{RwLock, RwLockUpgradableReadGuard};

use crate::session::HasSessionMetadata;
use crate::session_store::{SessionMetrics, SessionStore};

/// Used only when a caller doesn't pick a timeout explicitly via
/// `with_timeout` — e.g. the binary reading a `SESSION_TIMEOUT_SECS` env
/// var and falling back to this if it's unset.
const DEFAULT_SESSION_TIMEOUT: Duration = Duration::from_secs(60 * 10);

/// `get_handle` only bumps `last_accessed` if it's already at least this
/// stale, instead of unconditionally taking a write lock on every read.
/// A few seconds of slop against a many-minute idle timeout is
/// irrelevant to correctness, but avoiding the write lock for every
/// concurrent read of the same session removes real contention under
/// read-heavy load.
const TOUCH_GRANULARITY: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Metrics {
    created_sessions: AtomicU64,
    deleted_sessions: AtomicU64,
    expired_sessions: AtomicU64,
}

pub struct InMemorySessionStore<S> {
    sessions: Arc<RwLock<HashMap<String, Arc<RwLock<S>>>>>,
    metrics: Arc<Metrics>,
    timeout: Duration,
}

// Written by hand instead of `#[derive(Clone)]`: derive would add a
// `S: Clone` bound that isn't actually needed — every field here is
// already cheaply `Clone` via `Arc` (or `Copy`, for `timeout`) regardless
// of what `S` is.
impl<S> Clone for InMemorySessionStore<S> {
    fn clone(&self) -> Self {
        Self {
            sessions: self.sessions.clone(),
            metrics: self.metrics.clone(),
            timeout: self.timeout,
        }
    }
}

impl<S: HasSessionMetadata + Send + Sync + 'static> Default for InMemorySessionStore<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: HasSessionMetadata + Send + Sync + 'static> InMemorySessionStore<S> {
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_SESSION_TIMEOUT)
    }

    /// Builds a store with a custom idle timeout instead of the default
    /// 10 minutes — e.g. so the binary can make this configurable per
    /// deployment (an env var) without a code change here. Each tool's
    /// own store instance can pick its own timeout independently.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            metrics: Arc::new(Metrics::default()),
            timeout,
        }
    }

    pub fn start_cleanup_task(&self) {
        let sessions = self.sessions.clone();

        let metrics = self.metrics.clone();

        let timeout = self.timeout;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));

            loop {
                interval.tick().await;

                // A panic inside one tick must never end this loop
                // permanently — sessions would then stop expiring for the
                // rest of the process's life with no visible symptom
                // until memory usage or stale-session reports raise an
                // alarm. Catch it, log it, and keep ticking.
                let tick_result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    Self::cleanup_expired(&sessions, timeout)
                }));

                match tick_result {
                    Ok(expired) if expired > 0 => {
                        metrics
                            .expired_sessions
                            .fetch_add(expired as u64, Ordering::Relaxed);
                    }
                    Ok(_) => {}
                    Err(_) => {
                        tracing::error!(
                            "Session cleanup tick panicked; skipping this tick and continuing",
                        );
                    }
                }
            }
        });
    }

    /// Sweeps expired sessions in two passes so the map's write lock --
    /// which blocks every concurrent `save`/`get_handle`/`delete` call
    /// for every session, not just the ones actually being removed -- is
    /// only ever held long enough to remove the handful of IDs found
    /// expired, not for the whole O(n) scan across every session.
    ///
    /// Pass 1 (`scan_expired_candidates`) finds candidates under a READ
    /// lock on the map, so the scan itself never blocks a concurrent
    /// `save`/`get_handle`/`delete` on any session. Pass 2
    /// (`remove_still_expired`) then re-checks each candidate under that
    /// session's OWN write lock before removing it from the map, closing
    /// the gap where a concurrent `get_handle` bumps `last_accessed`
    /// (via that same per-session lock) between the scan and the
    /// removal -- without that re-check, a session touched in that
    /// window would be swept away anyway, silently discarding the
    /// access that just happened. Returns the number of sessions
    /// actually removed.
    fn cleanup_expired(
        sessions: &RwLock<HashMap<String, Arc<RwLock<S>>>>,
        timeout: Duration,
    ) -> usize {
        let now = SystemTime::now();

        let candidates = Self::scan_expired_candidates(sessions, now, timeout);

        Self::remove_still_expired(sessions, candidates, now, timeout)
    }

    /// Read-locks the map (not write-locks it) for the duration of the
    /// scan -- every concurrent `save`/`get_handle`/`delete` for any
    /// session can proceed uncontended while this runs. Each session's
    /// own read lock is taken only momentarily, one at a time, to check
    /// its `last_accessed`.
    fn scan_expired_candidates(
        sessions: &RwLock<HashMap<String, Arc<RwLock<S>>>>,
        now: SystemTime,
        timeout: Duration,
    ) -> Vec<(String, Arc<RwLock<S>>)> {
        let map = sessions.read();

        map.iter()
            .filter(|(_, handle)| is_expired(&*handle.read(), now, timeout))
            .map(|(id, handle)| (id.clone(), Arc::clone(handle)))
            .collect()
    }

    /// Takes the map's write lock only now, and only for as long as it
    /// takes to walk the (typically small) candidate list -- not the
    /// whole session table. Each candidate is re-checked under its own
    /// write lock immediately before removal, so a session that was
    /// touched (via `get_handle`) after the scan above but before this
    /// runs is correctly left in place instead of being removed anyway.
    fn remove_still_expired(
        sessions: &RwLock<HashMap<String, Arc<RwLock<S>>>>,
        candidates: Vec<(String, Arc<RwLock<S>>)>,
        now: SystemTime,
        timeout: Duration,
    ) -> usize {
        if candidates.is_empty() {
            return 0;
        }

        let mut map = sessions.write();

        let mut expired_count = 0usize;

        for (session_id, handle) in candidates {
            // Write-locking the session itself (rather than reading it
            // again) guarantees this re-check happens strictly after any
            // concurrent `get_handle` bump that had already started
            // taking this same per-session lock, instead of racing it.
            let (still_expired, age_ms) = {
                let session = handle.write();

                (
                    is_expired(&*session, now, timeout),
                    now.duration_since(session.metadata().created_at)
                        .unwrap_or_default()
                        .as_millis(),
                )
            };

            if !still_expired {
                continue;
            }

            if map.remove(&session_id).is_some() {
                tracing::info!(
                    session_id = %session_id,
                    age_ms,
                    "Session expired"
                );

                expired_count += 1;
            }
        }

        expired_count
    }
}

/// Shared by both cleanup passes so "expired" can never mean two
/// slightly different things depending on which pass is asking.
fn is_expired<S: HasSessionMetadata>(session: &S, now: SystemTime, timeout: Duration) -> bool {
    match now.duration_since(session.metadata().last_accessed) {
        Ok(elapsed) => elapsed > timeout,
        Err(_) => false,
    }
}

impl<S: HasSessionMetadata + Send + Sync + 'static> SessionStore<S> for InMemorySessionStore<S> {
    fn save(&self, mut session: S) {
        let mut sessions = self.sessions.write();

        session.metadata_mut().last_accessed = SystemTime::now();

        let id = session.metadata().id.clone();

        // `created_sessions` should count distinct sessions ever created,
        // not every call to `save` — today's call pattern only ever saves
        // a fresh UUID once, so this branch is currently always taken,
        // but the check is cheap insurance against a future caller
        // re-saving an existing id (a session-update pattern rather than
        // session-creation) silently inflating the metric.
        let is_new = !sessions.contains_key(&id);

        sessions.insert(id, Arc::new(RwLock::new(session)));

        if is_new {
            self.metrics
                .created_sessions
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn get_handle(&self, id: &str) -> Option<Arc<RwLock<S>>> {
        let sessions = self.sessions.read();

        let handle = sessions.get(id)?.clone();

        drop(sessions);

        // Only actually take the write lock if the timestamp is stale
        // enough to matter (see `TOUCH_GRANULARITY`) — an upgradable read
        // lets every concurrent reader of this same session check
        // staleness without blocking each other, and only the rare
        // "actually needs a bump" case pays for a write lock at all.
        // Scoped so the guard is dropped before `handle` moves out below.
        {
            let upgradable = handle.upgradable_read();

            let now = SystemTime::now();

            let stale = now
                .duration_since(upgradable.metadata().last_accessed)
                .map(|elapsed| elapsed >= TOUCH_GRANULARITY)
                .unwrap_or(true);

            if stale {
                let mut session = RwLockUpgradableReadGuard::upgrade(upgradable);

                session.metadata_mut().last_accessed = now;
            }
        }

        Some(handle)
    }

    fn delete(&self, id: &str) {
        let mut sessions = self.sessions.write();

        if sessions.remove(id).is_some() {
            self.metrics
                .deleted_sessions
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    fn metrics(&self) -> SessionMetrics {
        let active_sessions = self.sessions.read().len();

        SessionMetrics {
            active_sessions,
            created_sessions: self.metrics.created_sessions.load(Ordering::Relaxed),
            deleted_sessions: self.metrics.deleted_sessions.load(Ordering::Relaxed),
            expired_sessions: self.metrics.expired_sessions.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
#[path = "in_memory_session_store_tests.rs"]
mod tests;
