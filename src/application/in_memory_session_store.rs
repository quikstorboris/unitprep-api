use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{
    AtomicU64,
    Ordering,
};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::RwLock;

use crate::application::session_store::{
    SessionMetrics,
    SessionStore,
};
use crate::domain::session::Session;

const SESSION_TIMEOUT: Duration =
    Duration::from_secs(60 * 10);

#[derive(Default)]
struct Metrics {
    created_sessions: AtomicU64,
    deleted_sessions: AtomicU64,
    expired_sessions: AtomicU64,
}

#[derive(Clone)]
pub struct InMemorySessionStore {
    sessions: Arc<
        RwLock<
            HashMap<
                String,
                Arc<RwLock<Session>>,
            >,
        >,
    >,
    metrics: Arc<Metrics>,
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(
                RwLock::new(HashMap::new()),
            ),
            metrics: Arc::new(
                Metrics::default(),
            ),
        }
    }

    pub fn start_cleanup_task(
        &self,
    ) {
        let sessions =
            self.sessions.clone();

        let metrics =
            self.metrics.clone();

        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(
                    Duration::from_secs(60),
                );

            loop {
                interval.tick().await;

                // A panic inside one tick must never end this loop
                // permanently — sessions would then stop expiring for the
                // rest of the process's life with no visible symptom
                // until memory usage or stale-session reports raise an
                // alarm. Catch it, log it, and keep ticking.
                let tick_result =
                    std::panic::catch_unwind(
                        AssertUnwindSafe(|| {
                            let mut map =
                                sessions
                                    .write();

                            let before =
                                map.len();

                            Self::cleanup_expired(
                                &mut map,
                            );

                            before.saturating_sub(
                                map.len(),
                            )
                        }),
                    );

                match tick_result {
                    Ok(expired) if expired > 0 => {
                        metrics
                            .expired_sessions
                            .fetch_add(
                                expired as u64,
                                Ordering::Relaxed,
                            );
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

    fn cleanup_expired(
        sessions: &mut HashMap<
            String,
            Arc<RwLock<Session>>,
        >,
    ) {
        let now = SystemTime::now();

        sessions.retain(
            |session_id, session_handle| {
                let session =
                    session_handle
                        .read();

                let expired =
                    match now.duration_since(
                        session
                            .metadata
                            .last_accessed,
                    ) {
                        Ok(elapsed) => {
                            elapsed > SESSION_TIMEOUT
                        }
                        Err(_) => false,
                    };

                if expired {
                    let age_ms = now
                        .duration_since(
                            session
                                .metadata
                                .created_at,
                        )
                        .unwrap_or_default()
                        .as_millis();

                    tracing::info!(
                        session_id = %session_id,
                        age_ms,
                        "Session expired"
                    );
                }

                !expired
            },
        );
    }
}

impl SessionStore
    for InMemorySessionStore
{
    fn save(
        &self,
        mut session: Session,
    ) {
        let mut sessions =
            self.sessions
                .write();

        session.metadata.last_accessed =
            SystemTime::now();

        sessions.insert(
            session.metadata.id.clone(),
            Arc::new(
                RwLock::new(session),
            ),
        );

        self.metrics
            .created_sessions
            .fetch_add(
                1,
                Ordering::Relaxed,
            );
    }

    fn get_handle(
        &self,
        id: &str,
    ) -> Option<Arc<RwLock<Session>>> {
        let sessions =
            self.sessions
                .read();

        let handle =
            sessions.get(id)?.clone();

        {
            let mut session =
                handle
                    .write();

            session.metadata.last_accessed =
                SystemTime::now();
        }

        Some(handle)
    }

    fn delete(
        &self,
        id: &str,
    ) {
        let mut sessions =
            self.sessions
                .write();

        if sessions
            .remove(id)
            .is_some()
        {
            self.metrics
                .deleted_sessions
                .fetch_add(
                    1,
                    Ordering::Relaxed,
                );
        }
    }

    fn metrics(
        &self,
    ) -> SessionMetrics {
        let active_sessions =
            self.sessions
                .read()
                .len();

        SessionMetrics {
            active_sessions,
            created_sessions: self
                .metrics
                .created_sessions
                .load(
                    Ordering::Relaxed,
                ),
            deleted_sessions: self
                .metrics
                .deleted_sessions
                .load(
                    Ordering::Relaxed,
                ),
            expired_sessions: self
                .metrics
                .expired_sessions
                .load(
                    Ordering::Relaxed,
                ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_get_handle() {
        let store =
            InMemorySessionStore::new();

        let session =
            Session::new(
                "test-session"
                    .to_string(),
            );

        store.save(session);

        let result =
            store.get_handle(
                "test-session",
            );

        assert!(
            result.is_some()
        );
    }

    #[test]
    fn delete_removes_session() {
        let store =
            InMemorySessionStore::new();

        let session =
            Session::new(
                "test-session"
                    .to_string(),
            );

        store.save(session);

        store.delete(
            "test-session",
        );

        let result =
            store.get_handle(
                "test-session",
            );

        assert!(
            result.is_none()
        );
    }

    #[test]
    fn get_handle_returns_session() {
        let store =
            InMemorySessionStore::new();

        let session =
            Session::new(
                "test-session"
                    .to_string(),
            );

        store.save(session);

        let handle =
            store.get_handle(
                "test-session",
            );

        assert!(
            handle.is_some()
        );
    }

    #[test]
    fn metrics_track_created_sessions() {
        let store =
            InMemorySessionStore::new();

        store.save(
            Session::new(
                "s1".to_string(),
            ),
        );

        store.save(
            Session::new(
                "s2".to_string(),
            ),
        );

        let metrics =
            store.metrics();

        assert_eq!(
            metrics.created_sessions,
            2,
        );
    }

    #[test]
    fn metrics_track_deleted_sessions() {
        let store =
            InMemorySessionStore::new();

        store.save(
            Session::new(
                "s1".to_string(),
            ),
        );

        store.delete("s1");

        let metrics =
            store.metrics();

        assert_eq!(
            metrics.deleted_sessions,
            1,
        );
    }

    #[test]
    fn metrics_report_active_sessions() {
        let store =
            InMemorySessionStore::new();

        store.save(
            Session::new(
                "s1".to_string(),
            ),
        );

        store.save(
            Session::new(
                "s2".to_string(),
            ),
        );

        let metrics =
            store.metrics();

        assert_eq!(
            metrics.active_sessions,
            2,
        );
    }
}