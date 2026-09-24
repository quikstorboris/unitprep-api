use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::Serialize;
use sqlx::PgPool;

use crate::in_memory_session_store::InMemorySessionStore;
use crate::session::{HasSessionMetadata, SessionMetadata};
use crate::session_store::{SessionMetrics, SessionStore};

/// How often the Postgres-row expiry sweep runs -- independent of, and
/// deliberately the same cadence as, `InMemorySessionStore`'s own
/// `start_cleanup_task` interval. See that sweep's own doc comment on
/// `DurableSessionStore::start_cleanup_task` for why a *separate* sweep
/// is needed at all, rather than piggybacking on the in-memory one.
const DB_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// A write-through wrapper around `InMemorySessionStore<S>` that survives
/// a process restart by also persisting a serialized snapshot of every
/// session to a Postgres table (`auth.durable_sessions` -- see the
/// migration that creates it for the schema and RLS reasoning).
///
/// ## Why a wrapper, not a from-scratch Postgres-backed store
///
/// `SessionStore::get_handle` must keep returning a *shared, mutable*
/// `Arc<parking_lot::RwLock<S>>` -- every concurrent in-process caller
/// that already holds or fetches a handle for the same session id must
/// see the others' mutations immediately (see `session_store.rs`'s own
/// locking-invariant doc comment). A naive implementation that
/// deserialized a fresh `S` from Postgres on every `get_handle` call
/// would silently break that: two concurrent handles for the same id
/// would each hold their own independent copy and diverge the moment
/// either one was mutated, with no error or panic to reveal it.
///
/// So this type keeps an `InMemorySessionStore<S>` as its actual source
/// of truth for every session currently resident in this process --
/// `get_handle` on a hit never touches Postgres at all, identical to
/// `InMemorySessionStore` on its own. Postgres only enters the picture
/// on a `save` (write-through, so a restart has something to recover)
/// and on a `get_handle` *miss* (cold-start rehydration: load the row,
/// deserialize it, and re-insert it into the in-memory layer via that
/// layer's own `save` -- from that point on it behaves exactly like a
/// session that had never left memory, including being the same shared
/// handle for every subsequent concurrent caller).
///
/// ## Why bincode, not serde_json, for the persisted payload
///
/// Every session type this store holds (starting with
/// `RegistrationCeremony`/`AuthenticationCeremony`) is Rust-only end to
/// end: nothing outside this process ever reads `auth.durable_sessions`
/// directly, and correctness is verified by an integration test (see
/// `main` crate's `registration_ceremony` tests), not by a human
/// eyeballing the row. `serde_json` would have been the more
/// human-inspectable, `psql`-debuggable choice, but at a real cost here:
/// these sessions carry `webauthn_state: Vec<u8>` (webauthn-rs's own
/// opaque ceremony state), and JSON has no byte-string type -- serde_json
/// would encode that `Vec<u8>` as a JSON array of decimal numbers,
/// several times larger on the wire and on disk than bincode's compact
/// binary encoding of the exact same bytes for no benefit, since nobody
/// is meant to read that field anyway. Bincode wins on the actual
/// tradeoff that matters here.
///
/// ## Why timestamps cross the SQL boundary as epoch-second floats
///
/// The persisted metadata columns (`created_at`/`last_accessed`) are
/// real `TIMESTAMPTZ` columns, matching this project's own schema
/// conventions -- but this crate's own `chrono` dependency is
/// deliberately `default-features = false, features = ["alloc"]` (see
/// `Cargo.toml`), just enough for calamine's Excel date support, with no
/// `std`/`clock` feature that would be needed to convert a
/// `std::time::SystemTime` to a `chrono::DateTime` for sqlx binding.
/// Rather than widening that dependency (or adding sqlx's own
/// `chrono`/`time` feature) just for this, every timestamp crosses the
/// SQL boundary as a plain `f64` count of seconds since the Unix epoch:
/// written with Postgres's own `to_timestamp(...)`, read back with
/// `EXTRACT(EPOCH FROM ...)`. Both directions use only `float8`, which
/// sqlx supports natively with no extra feature flag.
pub struct DurableSessionStore<S> {
    inner: InMemorySessionStore<S>,
    db: PgPool,

    /// The `kind` discriminator column value this store instance reads
    /// and writes -- e.g. `"webauthn_registration_ceremony"`. One shared
    /// table serves every session type (see the migration's own doc
    /// comment for why), and this is what keeps them from colliding: two
    /// different `S` types are free to reuse the same session id space
    /// without ever reading or clobbering each other's rows.
    kind: String,

    timeout: Duration,
}

// Written by hand instead of `#[derive(Clone)]` for the same reason as
// `InMemorySessionStore`'s own manual `Clone` impl: every field here is
// already cheaply `Clone` (via `Arc`/`PgPool`'s internal `Arc`, or plain
// value types) regardless of what `S` is, so deriving would add an
// unnecessary `S: Clone` bound.
impl<S> Clone for DurableSessionStore<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            db: self.db.clone(),
            kind: self.kind.clone(),
            timeout: self.timeout,
        }
    }
}

impl<S> DurableSessionStore<S>
where
    S: HasSessionMetadata + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    /// `kind` should be a short, stable, unique-per-session-type string
    /// (see the field's own doc comment) -- it becomes part of this
    /// table's primary key, so changing it for an existing deployment
    /// would orphan whatever rows are already persisted under the old
    /// value.
    pub fn with_timeout(db: PgPool, kind: impl Into<String>, timeout: Duration) -> Self {
        Self {
            inner: InMemorySessionStore::with_timeout(timeout),
            db,
            kind: kind.into(),
            timeout,
        }
    }

    /// Starts BOTH cleanup mechanisms this store needs:
    ///
    /// 1. The inner `InMemorySessionStore`'s own hot-path sweep, entirely
    ///    unchanged -- it still expires sessions out of the in-process
    ///    map on the same schedule it always has.
    /// 2. A second, independent sweep that deletes this store's own
    ///    `kind`'s stale rows from `auth.durable_sessions`.
    ///
    /// These two must be separate rather than one piggybacking on the
    /// other's pass, because of *how* pass 1 actually expires a session:
    /// `InMemorySessionStore::cleanup_expired` removes the entry from its
    /// own private `HashMap` directly (see `in_memory_session_store.rs`)
    /// -- it has no reference back to this wrapper and so has no way to
    /// call `DurableSessionStore::delete` (or anything else) on the way
    /// out. If Postgres cleanup relied on that call happening, every
    /// session that expired via the passive idle-timeout sweep (as
    /// opposed to an explicit `delete()`, e.g. `/session/cancel`) would
    /// leave its row behind forever, and the table would grow
    /// unbounded -- exactly the failure this second sweep exists to
    /// prevent. Running it as its own periodic query (deleting by
    /// `kind` + `last_accessed` directly, no deserialization needed) is
    /// simpler than threading a callback through `InMemorySessionStore`
    /// and keeps that type's own contract (used as-is by the three tool
    /// sessions and every existing test) completely unchanged.
    pub fn start_cleanup_task(&self) {
        self.inner.start_cleanup_task();

        let db = self.db.clone();
        let kind = self.kind.clone();
        let timeout = self.timeout;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(DB_SWEEP_INTERVAL);

            loop {
                interval.tick().await;

                match delete_expired_rows(&db, &kind, timeout).await {
                    Ok(deleted) if deleted > 0 => {
                        tracing::info!(kind = %kind, deleted, "durable session Postgres sweep removed expired rows");
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            kind = %kind,
                            "durable session Postgres sweep failed; will retry on the next tick",
                        );
                    }
                }
            }
        });
    }

    /// Fire-and-forget write-through: serializes `session` and upserts it
    /// into Postgres on a spawned task rather than inline. `SessionStore::
    /// save` is a synchronous fn (a deliberate, existing part of the
    /// trait's contract -- see `session_store.rs` -- that this store must
    /// not change, since every business-logic call site calls it
    /// synchronously today), so it cannot itself `.await` the write.
    ///
    /// This does mean durability has a small window: if the process
    /// crashes between `save` returning and this spawned write actually
    /// completing, that particular save is lost on restart. That window
    /// is milliseconds wide and re-closes itself on the session's very
    /// next `save` (WebAuthn ceremonies call `save` once at `begin` and
    /// never again before `finish` deletes them, so in practice this
    /// only matters for a crash in the middle of `begin` itself) --
    /// accepted deliberately rather than blocking every request that
    /// touches a session on a network round trip to Postgres for a
    /// durability guarantee this table doesn't otherwise promise (there
    /// is no transactional coupling between "the HTTP response for
    /// begin() was sent" and "the ceremony survives a crash" today,
    /// unlike a real financial write).
    fn persist(&self, session: &S) {
        let payload = match bincode::serialize(session) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::error!(
                    error = %err,
                    kind = %self.kind,
                    session_id = %session.metadata().id,
                    "failed to serialize session for durable persistence -- the in-memory copy \
                     is still saved, but this session will not survive a process restart",
                );
                return;
            }
        };

        let metadata = session.metadata().clone();
        let db = self.db.clone();
        let kind = self.kind.clone();

        tokio::spawn(async move {
            if let Err(err) = upsert_row(&db, &kind, &metadata, payload).await {
                tracing::error!(
                    error = %err,
                    kind = %kind,
                    session_id = %metadata.id,
                    "failed to persist durable session to Postgres -- the in-memory copy is \
                     still saved, but this session will not survive a process restart until a \
                     later save succeeds",
                );
            }
        });
    }

    /// The cold-start path: only reached once `self.inner.get_handle`
    /// has already missed. Blocks on one Postgres round trip -- see the
    /// module doc comment's locking-invariant discussion below for why
    /// that's an acceptable, deliberate tradeoff here rather than
    /// something to avoid at all costs.
    ///
    /// `SessionStore::get_handle` is synchronous (same contract
    /// constraint as `save` above), but unlike `save`'s write this read
    /// cannot be fire-and-forget -- the whole point is to hand back a
    /// real handle from this very call. `tokio::task::block_in_place` +
    /// `Handle::block_on` is the sanctioned way to run async work to
    /// completion from inside sync code on a multi-threaded Tokio
    /// runtime: `block_in_place` moves this task off its worker thread
    /// so the blocking wait doesn't stall every other task queued on
    /// that same thread, which is exactly the failure mode a bare
    /// `Handle::block_on` (without it) risks. This only runs on a
    /// cache miss -- rare by construction (once per session per process
    /// lifetime, and only after a restart) -- so paying for a blocking
    /// round trip there, instead of making `SessionStore::get_handle`
    /// async and rewriting every call site across the app, is the right
    /// side of that tradeoff. Requires the "rt-multi-thread" Tokio
    /// feature (see Cargo.toml) and a multi-threaded runtime at the call
    /// site -- true for the real binary's `#[tokio::main]` and for any
    /// test using `#[tokio::test(flavor = "multi_thread")]`.
    fn rehydrate(&self, id: &str) -> Option<Arc<RwLock<S>>> {
        let db = self.db.clone();
        let kind = self.kind.clone();
        let id_owned = id.to_string();

        let fetched = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(fetch_row(&db, &kind, &id_owned))
        });

        let row = match fetched {
            Ok(Some(row)) => row,
            Ok(None) => return None,
            Err(err) => {
                tracing::error!(
                    error = %err,
                    kind = %kind,
                    session_id = %id_owned,
                    "failed to query Postgres for durable session rehydration",
                );
                return None;
            }
        };

        // Defensive: a persisted row can legitimately be stale if the
        // process was down for longer than `timeout` before this lookup
        // -- neither sweep (in-memory or Postgres) ever got a chance to
        // run against it in the meantime. Treat it exactly like a miss,
        // and clean the stale row up on the way out rather than leaving
        // it for the next periodic sweep to find, since we're already
        // here.
        if is_stale(row.last_accessed, self.timeout) {
            let db = db.clone();
            let kind = kind.clone();
            let id_owned = id_owned.clone();

            tokio::spawn(async move {
                if let Err(err) = delete_row(&db, &kind, &id_owned).await {
                    tracing::error!(
                        error = %err,
                        kind = %kind,
                        session_id = %id_owned,
                        "failed to delete stale durable session row found at rehydration",
                    );
                }
            });

            return None;
        }

        let session: S = match bincode::deserialize(&row.payload) {
            Ok(session) => session,
            Err(err) => {
                tracing::error!(
                    error = %err,
                    kind = %kind,
                    session_id = %id_owned,
                    "failed to deserialize persisted durable session -- treating as a miss",
                );
                return None;
            }
        };

        // Re-insert via the INNER store's own `save`, not `self.save` --
        // the row we just loaded this from is already correct and
        // current in Postgres, so re-persisting it (self.save's
        // write-through) would just be a redundant round trip.
        self.inner.save(session);

        self.inner.get_handle(id)
    }
}

impl<S> SessionStore<S> for DurableSessionStore<S>
where
    S: HasSessionMetadata + Serialize + DeserializeOwned + Send + Sync + 'static,
{
    fn save(&self, session: S) {
        self.persist(&session);

        self.inner.save(session);
    }

    fn get_handle(&self, id: &str) -> Option<Arc<RwLock<S>>> {
        if let Some(handle) = self.inner.get_handle(id) {
            return Some(handle);
        }

        self.rehydrate(id)
    }

    fn delete(&self, id: &str) {
        self.inner.delete(id);

        let db = self.db.clone();
        let kind = self.kind.clone();
        let id_owned = id.to_string();

        tokio::spawn(async move {
            if let Err(err) = delete_row(&db, &kind, &id_owned).await {
                tracing::error!(
                    error = %err,
                    kind = %kind,
                    session_id = %id_owned,
                    "failed to delete durable session row from Postgres",
                );
            }
        });
    }

    fn metrics(&self) -> SessionMetrics {
        // Deliberately the in-memory layer's own metrics, unchanged --
        // these count sessions actually resident in THIS process, which
        // is what every existing consumer of `SessionMetrics` (health/
        // ops output) means by "active sessions" today. A row sitting in
        // Postgres for a since-restarted process isn't "active" in that
        // sense until something actually rehydrates it.
        self.inner.metrics()
    }
}

struct PersistedRow {
    last_accessed: SystemTime,
    payload: Vec<u8>,
}

fn is_stale(last_accessed: SystemTime, timeout: Duration) -> bool {
    match SystemTime::now().duration_since(last_accessed) {
        Ok(elapsed) => elapsed > timeout,
        Err(_) => false,
    }
}

/// Seconds since the Unix epoch, as a plain `f64` -- see the module doc
/// comment for why timestamps cross the SQL boundary this way. A
/// `SystemTime` before the epoch (never true for a real session
/// timestamp) falls back to `0.0` rather than panicking.
fn epoch_seconds(time: SystemTime) -> f64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

async fn upsert_row(
    db: &PgPool,
    kind: &str,
    metadata: &SessionMetadata,
    payload: Vec<u8>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO auth.durable_sessions (id, kind, owner_id, created_at, last_accessed, cancelled, payload)
         VALUES ($1, $2, $3, to_timestamp($4), to_timestamp($5), $6, $7)
         ON CONFLICT (kind, id) DO UPDATE SET
             owner_id = EXCLUDED.owner_id,
             last_accessed = EXCLUDED.last_accessed,
             cancelled = EXCLUDED.cancelled,
             payload = EXCLUDED.payload",
    )
    .bind(&metadata.id)
    .bind(kind)
    .bind(metadata.owner_id)
    .bind(epoch_seconds(metadata.created_at))
    .bind(epoch_seconds(metadata.last_accessed))
    .bind(metadata.cancelled)
    .bind(payload)
    .execute(db)
    .await?;

    Ok(())
}

async fn fetch_row(db: &PgPool, kind: &str, id: &str) -> Result<Option<PersistedRow>, sqlx::Error> {
    use sqlx::Row;

    let row = sqlx::query(
        "SELECT EXTRACT(EPOCH FROM last_accessed)::float8 AS last_accessed_epoch, payload
         FROM auth.durable_sessions
         WHERE kind = $1 AND id = $2",
    )
    .bind(kind)
    .bind(id)
    .fetch_optional(db)
    .await?;

    Ok(row.map(|row| {
        let last_accessed_epoch: f64 = row.get("last_accessed_epoch");

        PersistedRow {
            last_accessed: UNIX_EPOCH + Duration::from_secs_f64(last_accessed_epoch.max(0.0)),
            payload: row.get("payload"),
        }
    }))
}

async fn delete_row(db: &PgPool, kind: &str, id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM auth.durable_sessions WHERE kind = $1 AND id = $2")
        .bind(kind)
        .bind(id)
        .execute(db)
        .await?;

    Ok(())
}

async fn delete_expired_rows(
    db: &PgPool,
    kind: &str,
    timeout: Duration,
) -> Result<u64, sqlx::Error> {
    let cutoff = epoch_seconds(SystemTime::now()) - timeout.as_secs_f64();

    let result = sqlx::query(
        "DELETE FROM auth.durable_sessions
         WHERE kind = $1 AND last_accessed < to_timestamp($2)",
    )
    .bind(kind)
    .bind(cutoff)
    .execute(db)
    .await?;

    Ok(result.rows_affected())
}

#[cfg(test)]
#[path = "durable_session_store_tests.rs"]
mod tests;
