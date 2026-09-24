use uuid::Uuid;

use crate::session_store::SessionStoreExt;

use super::*;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

/// A `PgPool` that is real enough to type-check and construct, but never
/// actually reaches Postgres in these tests. `connect_lazy` defers the
/// TCP connection until first use (see `db.rs`'s own use of the `_with`
/// sibling), and every test below only ever exercises the in-memory
/// HIT path -- exactly the point of these tests, matching this crate's
/// `in_memory_session_store_tests.rs` in density/style, with no
/// behavior change from `InMemorySessionStore` asserted here. The rare
/// miss/rehydration path (which WOULD need a real, reachable Postgres)
/// is covered separately by a real-database integration test in the
/// main crate against `RegistrationCeremony`, not here -- see that
/// test's own doc comment for why a real DB is unavoidable there.
///
/// Port 1 on loopback is deliberate: nothing listens there, so a
/// connection attempt (if one ever happened) fails immediately
/// ("connection refused") rather than hanging on a DNS lookup or a
/// firewall timeout the way an arbitrary unreachable host might --
/// this keeps the test suite fast even if a future test accidentally
/// exercises a code path that does touch the pool.
fn unreachable_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://postgres:postgres@127.0.0.1:1/postgres")
        .expect("connect_lazy must not fail synchronously on a well-formed URL")
}

fn store<S>(kind: &str) -> DurableSessionStore<S>
where
    S: HasSessionMetadata + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    DurableSessionStore::with_timeout(unreachable_pool(), kind, Duration::from_secs(600))
}

#[tokio::test]
async fn save_and_get_handle_returns_the_saved_session_from_the_in_memory_hit_path() {
    let store: DurableSessionStore<TestSession> = store("test");

    store.save(TestSession::new("s1"));

    let handle = store.get_handle("s1");

    assert!(handle.is_some());
    assert_eq!(handle.unwrap().read().metadata.id, "s1");
}

#[tokio::test]
async fn delete_removes_the_session_from_the_in_memory_layer() {
    let store: DurableSessionStore<TestSession> = store("test");

    store.save(TestSession::new("s1"));
    store.delete("s1");

    // Checked against the INNER store directly (not `store.get_handle`)
    // -- a lookup through the wrapper after a delete is a genuine miss,
    // which would fall through to `rehydrate` and attempt a real
    // Postgres round trip against `unreachable_pool()`. Reaching into
    // `inner` is possible here (this test module is a child of
    // `durable_session_store`, and Rust privacy is scoped to the module
    // tree) and keeps this test on the in-memory hit/removal path only,
    // matching what it's meant to prove: the wrapper's `delete` does
    // remove the in-memory entry, same as `InMemorySessionStore` alone.
    assert!(store.inner.get_handle("s1").is_none());
}

#[tokio::test]
async fn metrics_track_created_sessions() {
    let store: DurableSessionStore<TestSession> = store("test");

    store.save(TestSession::new("s1"));
    store.save(TestSession::new("s2"));

    assert_eq!(store.metrics().created_sessions, 2);
}

#[tokio::test]
async fn metrics_do_not_double_count_created_sessions_on_overwrite() {
    let store: DurableSessionStore<TestSession> = store("test");

    store.save(TestSession::new("s1"));
    store.save(TestSession::new("s1"));

    assert_eq!(store.metrics().created_sessions, 1);
}

#[tokio::test]
async fn metrics_track_deleted_sessions() {
    let store: DurableSessionStore<TestSession> = store("test");

    store.save(TestSession::new("s1"));
    store.delete("s1");

    assert_eq!(store.metrics().deleted_sessions, 1);
}

#[tokio::test]
async fn with_owned_session_succeeds_for_the_matching_owner_through_the_wrapper() {
    let store: DurableSessionStore<TestSession> = store("test");
    let owner = Uuid::new_v4();

    store.save(TestSession::owned_by("s1", owner));

    let result = store.with_owned_session("s1", owner, |_| "ok");

    assert_eq!(result, Some("ok"));
}

#[tokio::test]
async fn with_owned_session_returns_none_for_a_mismatched_owner_through_the_wrapper() {
    let store: DurableSessionStore<TestSession> = store("test");

    store.save(TestSession::owned_by("s1", Uuid::new_v4()));

    let result = store.with_owned_session("s1", Uuid::new_v4(), |_| "ok");

    assert_eq!(result, None);
}

/// Same reasoning as `session.rs`'s own `SessionMetadata` derive doc
/// comment: `bincode::serialize` must actually succeed for a real
/// session shape (a `String`, two `SystemTime`s, an `Option<Uuid>`, a
/// `bool`) before this store is ever wired to a real ceremony type --
/// this is that assumption, checked directly, independent of Postgres
/// being reachable at all.
#[test]
fn a_typical_session_round_trips_through_bincode() {
    let original = TestSession::owned_by("s1", Uuid::new_v4());

    let payload = bincode::serialize(&original).expect("serialize must succeed");
    let restored: TestSession = bincode::deserialize(&payload).expect("deserialize must succeed");

    assert_eq!(restored.metadata.id, original.metadata.id);
    assert_eq!(restored.metadata.owner_id, original.metadata.owner_id);
}
