use uuid::Uuid;

use crate::session_store::SessionStoreExt;

use super::*;

#[derive(Debug, Clone)]
struct TestSession {
    metadata: crate::session::SessionMetadata,
}

impl TestSession {
    fn new(id: &str) -> Self {
        Self {
            metadata: crate::session::SessionMetadata::new(id.to_string(), None),
        }
    }

    fn owned_by(id: &str, owner_id: Uuid) -> Self {
        Self {
            metadata: crate::session::SessionMetadata::new(id.to_string(), Some(owner_id)),
        }
    }
}

impl HasSessionMetadata for TestSession {
    fn metadata(&self) -> &crate::session::SessionMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut crate::session::SessionMetadata {
        &mut self.metadata
    }
}

#[test]
fn save_and_get_handle() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();

    store.save(TestSession::new("test-session"));

    let result = store.get_handle("test-session");

    assert!(result.is_some());
}

#[test]
fn delete_removes_session() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();

    store.save(TestSession::new("test-session"));

    store.delete("test-session");

    let result = store.get_handle("test-session");

    assert!(result.is_none());
}

#[test]
fn get_handle_returns_session() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();

    store.save(TestSession::new("test-session"));

    let handle = store.get_handle("test-session");

    assert!(handle.is_some());
}

#[test]
fn metrics_track_created_sessions() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();

    store.save(TestSession::new("s1"));
    store.save(TestSession::new("s2"));

    let metrics = store.metrics();

    assert_eq!(metrics.created_sessions, 2,);
}

#[test]
fn metrics_track_deleted_sessions() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();

    store.save(TestSession::new("s1"));
    store.delete("s1");

    let metrics = store.metrics();

    assert_eq!(metrics.deleted_sessions, 1,);
}

#[test]
fn metrics_report_active_sessions() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();

    store.save(TestSession::new("s1"));
    store.save(TestSession::new("s2"));

    let metrics = store.metrics();

    assert_eq!(metrics.active_sessions, 2,);
}

/// Regression test for the metrics-correctness fix: `save`-ing an id
/// that already exists (an overwrite, not a creation) must not
/// double-count it as a second created session.
#[test]
fn metrics_do_not_double_count_created_sessions_on_overwrite() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();

    store.save(TestSession::new("s1"));
    store.save(TestSession::new("s1"));

    let metrics = store.metrics();

    assert_eq!(metrics.created_sessions, 1,);
}

/// Regression test for the `get_handle` throttled-touch fix: two
/// accesses in quick succession (well within `TOUCH_GRANULARITY`)
/// must not re-bump `last_accessed` the second time — proving the
/// write-lock skip actually happens, not just that the value is
/// merely "close enough."
#[test]
fn get_handle_does_not_bump_last_accessed_within_touch_granularity() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();

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
fn get_handle_bumps_last_accessed_once_stale() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();

    store.save(TestSession::new("s1"));

    let handle = store.get_handle("s1").unwrap();

    let backdated = SystemTime::now() - TOUCH_GRANULARITY - Duration::from_secs(1);

    handle.write().metadata_mut().last_accessed = backdated;

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
fn cleanup_expired_honors_a_custom_timeout() {
    let mut sessions: HashMap<String, Arc<RwLock<TestSession>>> = HashMap::new();

    let mut session = TestSession::new("s1");

    session.metadata_mut().last_accessed = SystemTime::now() - Duration::from_secs(5);

    sessions.insert("s1".to_string(), Arc::new(RwLock::new(session)));

    InMemorySessionStore::<TestSession>::cleanup_expired(&mut sessions, Duration::from_secs(1));

    assert!(sessions.is_empty());
}

/// `with_owned_session` must succeed when the caller actually owns the
/// session — the normal case once auth is wired in.
#[test]
fn with_owned_session_succeeds_for_the_matching_owner() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();
    let owner = Uuid::new_v4();

    store.save(TestSession::owned_by("s1", owner));

    let result = store.with_owned_session("s1", owner, |_| "ok");

    assert_eq!(result, Some("ok"));
}

/// The core of the whole mechanism: a session that exists but belongs to
/// someone else must come back exactly like a nonexistent one -- `None`,
/// not a distinct "forbidden" result -- so a caller can never use it to
/// tell the difference between "not yours" and "doesn't exist."
#[test]
fn with_owned_session_returns_none_for_a_mismatched_owner() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();

    store.save(TestSession::owned_by("s1", Uuid::new_v4()));

    let result = store.with_owned_session("s1", Uuid::new_v4(), |_| "ok");

    assert_eq!(result, None);
}

/// Same `None` result for a session id that was never saved at all --
/// matching `with_owned_session_returns_none_for_a_mismatched_owner`
/// above, confirming the two cases are genuinely indistinguishable.
#[test]
fn with_owned_session_returns_none_for_a_nonexistent_session() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();

    let result = store.with_owned_session("nonexistent", Uuid::new_v4(), |_| "ok");

    assert_eq!(result, None);
}

/// A session created without an owner (`None` -- every real session
/// today, since no endpoint has an authenticated caller yet) must not be
/// claimable by any caller-supplied id -- `Some(owner) != None` should
/// never accidentally compare equal to anything.
#[test]
fn with_owned_session_returns_none_when_the_session_has_no_owner() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();

    store.save(TestSession::new("s1"));

    let result = store.with_owned_session("s1", Uuid::new_v4(), |_| "ok");

    assert_eq!(result, None);
}

/// Mutable counterpart -- same ownership gate, but through
/// `with_owned_session_mut`, proving the write path enforces it too.
#[test]
fn with_owned_session_mut_only_applies_the_mutation_for_the_matching_owner() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();
    let owner = Uuid::new_v4();

    store.save(TestSession::owned_by("s1", owner));

    let wrong_owner_result = store.with_owned_session_mut("s1", Uuid::new_v4(), |session| {
        session.metadata_mut().last_accessed = SystemTime::now();
    });

    assert_eq!(wrong_owner_result, None);

    let right_owner_result = store.with_owned_session_mut("s1", owner, |_| "ok");

    assert_eq!(right_owner_result, Some("ok"));
}
