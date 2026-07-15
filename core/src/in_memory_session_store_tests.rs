use super::*;

#[derive(Debug, Clone)]
struct TestSession {
    metadata: crate::session::SessionMetadata,
}

impl TestSession {
    fn new(id: &str) -> Self {
        Self {
            metadata: crate::session::SessionMetadata::new(
                id.to_string(),
            ),
        }
    }
}

impl HasSessionMetadata for TestSession {
    fn metadata(&self) -> &crate::session::SessionMetadata {
        &self.metadata
    }

    fn metadata_mut(
        &mut self,
    ) -> &mut crate::session::SessionMetadata {
        &mut self.metadata
    }
}

#[test]
fn save_and_get_handle() {
    let store: InMemorySessionStore<TestSession> =
        InMemorySessionStore::new();

    store.save(TestSession::new("test-session"));

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
    let store: InMemorySessionStore<TestSession> =
        InMemorySessionStore::new();

    store.save(TestSession::new("test-session"));

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
    let store: InMemorySessionStore<TestSession> =
        InMemorySessionStore::new();

    store.save(TestSession::new("test-session"));

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
    let store: InMemorySessionStore<TestSession> =
        InMemorySessionStore::new();

    store.save(TestSession::new("s1"));
    store.save(TestSession::new("s2"));

    let metrics =
        store.metrics();

    assert_eq!(
        metrics.created_sessions,
        2,
    );
}

#[test]
fn metrics_track_deleted_sessions() {
    let store: InMemorySessionStore<TestSession> =
        InMemorySessionStore::new();

    store.save(TestSession::new("s1"));
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
    let store: InMemorySessionStore<TestSession> =
        InMemorySessionStore::new();

    store.save(TestSession::new("s1"));
    store.save(TestSession::new("s2"));

    let metrics =
        store.metrics();

    assert_eq!(
        metrics.active_sessions,
        2,
    );
}

/// Regression test for the metrics-correctness fix: `save`-ing an id
/// that already exists (an overwrite, not a creation) must not
/// double-count it as a second created session.
#[test]
fn metrics_do_not_double_count_created_sessions_on_overwrite(
) {
    let store: InMemorySessionStore<TestSession> =
        InMemorySessionStore::new();

    store.save(TestSession::new("s1"));
    store.save(TestSession::new("s1"));

    let metrics =
        store.metrics();

    assert_eq!(
        metrics.created_sessions,
        1,
    );
}

/// Regression test for the `get_handle` throttled-touch fix: two
/// accesses in quick succession (well within `TOUCH_GRANULARITY`)
/// must not re-bump `last_accessed` the second time — proving the
/// write-lock skip actually happens, not just that the value is
/// merely "close enough."
#[test]
fn get_handle_does_not_bump_last_accessed_within_touch_granularity(
) {
    let store: InMemorySessionStore<TestSession> =
        InMemorySessionStore::new();

    store.save(TestSession::new("s1"));

    let first = store
        .get_handle("s1")
        .unwrap()
        .read()
        .metadata()
        .last_accessed;

    let second = store
        .get_handle("s1")
        .unwrap()
        .read()
        .metadata()
        .last_accessed;

    assert_eq!(first, second);
}

/// Regression test for the same fix's other branch: once
/// `last_accessed` is actually stale (older than
/// `TOUCH_GRANULARITY`), `get_handle` must still bump it —
/// the throttle must not silently turn into "never touch."
#[test]
fn get_handle_bumps_last_accessed_once_stale(
) {
    let store: InMemorySessionStore<TestSession> =
        InMemorySessionStore::new();

    store.save(TestSession::new("s1"));

    let handle = store
        .get_handle("s1")
        .unwrap();

    let backdated = SystemTime::now()
        - TOUCH_GRANULARITY
        - Duration::from_secs(1);

    handle
        .write()
        .metadata_mut()
        .last_accessed = backdated;

    let refreshed = store
        .get_handle("s1")
        .unwrap()
        .read()
        .metadata()
        .last_accessed;

    assert!(refreshed > backdated);
}

/// Regression test for the configurable-timeout fix: `cleanup_expired`
/// must actually honor a custom `timeout` value, not the hardcoded
/// default — a session backdated past a short custom timeout must be
/// removed even though it wouldn't be past the 10-minute default.
#[test]
fn cleanup_expired_honors_a_custom_timeout(
) {
    let mut sessions: HashMap<
        String,
        Arc<RwLock<TestSession>>,
    > = HashMap::new();

    let mut session =
        TestSession::new("s1");

    session
        .metadata_mut()
        .last_accessed = SystemTime::now()
        - Duration::from_secs(5);

    sessions.insert(
        "s1".to_string(),
        Arc::new(RwLock::new(
            session,
        )),
    );

    InMemorySessionStore::<TestSession>::cleanup_expired(
        &mut sessions,
        Duration::from_secs(1),
    );

    assert!(sessions.is_empty());
}
