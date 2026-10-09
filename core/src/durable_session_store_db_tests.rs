//! Real-database tests for the scheduled Postgres expiry sweep (efficiency
//! follow-up, 2026-10-09: the sweep used to be a 60-second timer per store,
//! which kept a scale-to-zero database awake forever).
//!
//! `#[ignore]`d and named `_db_` so CI's convention-based step runs them
//! against the ephemeral test-db. They need only `TEST_DATABASE_URL`:
//!
//! ```text
//! TEST_DATABASE_URL=postgres://app_service:app_service@127.0.0.1:5433/unitprep_test \
//!   cargo test -p unitprep-core -- --ignored durable_sweep_db
//! ```
//!
//! Each test uses its own `kind`, so tests never see each other's rows.

use std::time::{Duration, Instant};

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

use super::*;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Probe {
    metadata: crate::session::SessionMetadata,
}

impl Probe {
    fn new(id: &str) -> Self {
        Self {
            metadata: crate::session::SessionMetadata::new(id.to_string(), None),
        }
    }
}

impl HasSessionMetadata for Probe {
    fn metadata(&self) -> &crate::session::SessionMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut crate::session::SessionMetadata {
        &mut self.metadata
    }
}

fn test_pool() -> PgPool {
    let url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point at the local test-db");
    assert!(
        !url.contains("neon.tech"),
        "refusing to run against Neon: point TEST_DATABASE_URL at the ephemeral test-db"
    );
    PgPoolOptions::new()
        .max_connections(3)
        .connect_lazy(&url)
        .expect("TEST_DATABASE_URL must be well-formed")
}

fn unique_kind() -> String {
    format!("sweep-db-{}", Uuid::new_v4())
}

async fn row_count(db: &PgPool, kind: &str) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM auth.durable_sessions WHERE kind = $1")
        .bind(kind)
        .fetch_one(db)
        .await
        .expect("count rows")
}

/// Polls `condition` every 50 ms until it holds or `within` passes.
async fn eventually<F, Fut>(within: Duration, mut condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + within;
    loop {
        if condition().await {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn durable_sweep_db_removes_an_expired_row_and_then_goes_quiet() {
    let db = test_pool();
    let kind = unique_kind();
    let store = DurableSessionStore::<Probe>::with_timeout(
        db.clone(),
        kind.clone(),
        Duration::from_secs(1),
    )
    .with_sweep_timing(SweepTiming {
        grace: Duration::from_millis(200),
        // Far away: this test is about the write-driven schedule alone.
        startup_delay: Duration::from_secs(600),
    });
    store.start_cleanup_task();

    store.save(Probe::new("expires-soon"));

    assert!(
        eventually(Duration::from_secs(3), || async {
            row_count(&db, &kind).await == 1
        })
        .await,
        "the write-through must reach Postgres"
    );
    assert!(
        !store.sweep_is_idle(),
        "a stored row must have a sweep scheduled for just after its expiry"
    );

    assert!(
        eventually(Duration::from_secs(8), || async {
            row_count(&db, &kind).await == 0
        })
        .await,
        "the scheduled sweep must remove the expired row"
    );
    assert!(
        eventually(Duration::from_secs(2), || async { store.sweep_is_idle() }).await,
        "with no rows left nothing may stay scheduled, so the database can sleep"
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn durable_sweep_db_clears_rows_left_over_from_before_a_restart_and_then_goes_quiet() {
    let db = test_pool();
    let kind = unique_kind();

    // A row an earlier process wrote an hour ago and never got to sweep.
    sqlx::query(
        "INSERT INTO auth.durable_sessions (id, kind, owner_id, created_at, last_accessed, payload)
         VALUES ('leftover', $1, NULL, now() - interval '1 hour', now() - interval '1 hour', '\\x00')",
    )
    .bind(&kind)
    .execute(&db)
    .await
    .expect("seed a stale row");
    assert_eq!(row_count(&db, &kind).await, 1);

    let store = DurableSessionStore::<Probe>::with_timeout(
        db.clone(),
        kind.clone(),
        Duration::from_secs(600),
    )
    .with_sweep_timing(SweepTiming {
        grace: Duration::from_millis(200),
        startup_delay: Duration::from_millis(100),
    });
    store.start_cleanup_task();

    assert!(
        eventually(Duration::from_secs(5), || async {
            row_count(&db, &kind).await == 0
        })
        .await,
        "the startup sweep must clear rows left by the previous process"
    );
    assert!(
        eventually(Duration::from_secs(2), || async { store.sweep_is_idle() }).await,
        "and then nothing stays scheduled"
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn durable_sweep_db_an_idle_store_never_touches_the_database() {
    // The old design ran its first sweep the moment the task started and then
    // every 60 seconds, so the pool would have opened a connection within
    // milliseconds. A lazily-connecting pool that is still empty proves the
    // store made no query on its own.
    let db = test_pool();
    let store = DurableSessionStore::<Probe>::with_timeout(
        db.clone(),
        unique_kind(),
        Duration::from_secs(600),
    )
    .with_sweep_timing(SweepTiming {
        grace: Duration::from_secs(300),
        startup_delay: Duration::from_secs(600),
    });
    store.start_cleanup_task();

    tokio::time::sleep(Duration::from_millis(1500)).await;

    assert_eq!(
        db.size(),
        0,
        "no sweep may have run (and so no connection opened) before one is due"
    );
}
