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
    let sessions: RwLock<HashMap<String, Arc<RwLock<TestSession>>>> = RwLock::new(HashMap::new());

    let mut session = TestSession::new("s1");

    session.metadata_mut().last_accessed = SystemTime::now() - Duration::from_secs(5);

    sessions
        .write()
        .insert("s1".to_string(), Arc::new(RwLock::new(session)));

    let expired =
        InMemorySessionStore::<TestSession>::cleanup_expired(&sessions, Duration::from_secs(1));

    assert_eq!(expired, 1);
    assert!(sessions.read().is_empty());
}

/// Regression test for the lock-scope fix: `cleanup_expired` used to
/// take the whole map's write lock for its entire O(n) scan, which also
/// meant a session found expired during the scan was removed
/// unconditionally, even if something touched it (via `get_handle`,
/// which bumps `last_accessed`) in the gap between the scan and the
/// actual removal. This drives the two passes directly to simulate
/// exactly that gap deterministically, without relying on real thread
/// scheduling: scan finds "s1" expired, `get_handle` then bumps it (as
/// a concurrent handler would), and only then does the removal pass
/// run against the stale candidate list -- it must re-check and leave
/// "s1" in place rather than removing it anyway.
#[test]
fn touching_a_session_between_scan_and_removal_survives_the_sweep() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();

    store.save(TestSession::new("s1"));

    // Backdate past a short custom timeout so the scan below finds it
    // expired, the same way a real idle session would be found.
    store
        .get_handle("s1")
        .unwrap()
        .write()
        .metadata_mut()
        .last_accessed = SystemTime::now() - Duration::from_secs(5);

    let timeout = Duration::from_secs(1);
    let now = SystemTime::now();

    let candidates =
        InMemorySessionStore::<TestSession>::scan_expired_candidates(&store.sessions, now, timeout);

    assert_eq!(candidates.len(), 1, "scan should have found \"s1\" expired");

    // Simulate a concurrent handler calling `get_handle` in the window
    // between the scan and the removal pass -- this bumps
    // `last_accessed` to "just now" via the session's own write lock,
    // exactly like a real in-flight request would.
    store.get_handle("s1");

    let expired_count = InMemorySessionStore::<TestSession>::remove_still_expired(
        &store.sessions,
        candidates,
        now,
        timeout,
    );

    assert_eq!(
        expired_count, 0,
        "the touched session must not be counted as removed"
    );

    assert!(
        store.get_handle("s1").is_some(),
        "a session touched between scan and removal must survive the sweep"
    );
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

/// Regression test for the cancel/concurrent-mutation-race fix: once a
/// session's `cancelled` flag is set, every one of `SessionStoreExt`'s
/// generic access methods must treat it exactly like a nonexistent
/// session -- `None`, not a distinct "cancelled" result -- same
/// reasoning as the `owner_id` mismatch gate above. Deliberately keeps a
/// raw `Arc` handle (from `get_handle`) alive across the whole test,
/// simulating a concurrent caller that already checked out its own
/// handle to this session before it was cancelled -- proving the gate
/// lives in the shared access layer (these four methods), not in
/// whether the underlying object still happens to exist or be reachable
/// by some other route.
#[test]
fn cancelled_session_is_unreachable_through_every_generic_access_method() {
    let store: InMemorySessionStore<TestSession> = InMemorySessionStore::new();
    let owner = Uuid::new_v4();

    store.save(TestSession::owned_by("s1", owner));

    let held_handle = store.get_handle("s1").unwrap();

    held_handle.write().metadata_mut().cancelled = true;

    assert_eq!(store.with_session("s1", |_| "ok"), None);
    assert_eq!(store.with_session_mut("s1", |_| "ok"), None);
    assert_eq!(store.with_owned_session("s1", owner, |_| "ok"), None);
    assert_eq!(store.with_owned_session_mut("s1", owner, |_| "ok"), None);

    // Still technically alive and still in the map (no `delete` call
    // anywhere in this test) -- confirming the gate really is the
    // `cancelled` flag itself, not the session having been removed.
    assert!(held_handle.read().metadata().cancelled);
}

/// Genuine concurrency regression test for the same fix: real OS threads
/// racing for the same session's write lock, one mirroring
/// `cancel_session`'s handler (mark cancelled, then delete) and the
/// other mirroring any other handler mutating the same session (e.g.
/// `/correct`, via `with_session_mut`). Whichever thread's lock
/// acquisition wins a given run is left to real scheduling -- not
/// asserted -- but the OUTCOME must be deterministic every single run,
/// across many repetitions: no panic, no deadlock, and the session ends
/// up fully and cleanly gone either way, proving the mutator's write (if
/// it landed at all) never leaves a detached, still-mutable object that
/// silently outlives cancellation with no way to observe or reach it
/// again.
#[test]
fn concurrent_cancel_and_mutate_never_silently_loses_a_write() {
    for _ in 0..200 {
        let store: std::sync::Arc<InMemorySessionStore<TestSession>> =
            std::sync::Arc::new(InMemorySessionStore::new());

        store.save(TestSession::new("s1"));

        let canceller_store = std::sync::Arc::clone(&store);
        let mutator_store = std::sync::Arc::clone(&store);

        // Mirrors `cancel_session`'s handler exactly: mark cancelled
        // under the session's own write lock (via `with_session_mut`),
        // then remove the map entry only afterward.
        let canceller = std::thread::spawn(move || {
            if canceller_store
                .with_session_mut("s1", |session| {
                    session.metadata_mut().cancelled = true;
                })
                .is_some()
            {
                canceller_store.delete("s1");
            }
        });

        // Mirrors any other handler racing to mutate the same session
        // concurrently.
        let mutator = std::thread::spawn(move || {
            mutator_store.with_session_mut("s1", |session| {
                session.metadata_mut().last_accessed = SystemTime::now();
            })
        });

        canceller.join().expect("canceller thread must not panic");
        mutator.join().expect("mutator thread must not panic");

        // Regardless of which thread won the race this run, the session
        // must end up fully unreachable -- through the generic access
        // methods AND through a fresh `get_handle` -- never left in some
        // in-between state (e.g. cancelled but not yet deleted, visible
        // to one path but not the other).
        assert!(store.with_session("s1", |_| ()).is_none());
        assert!(store.get_handle("s1").is_none());
    }
}
