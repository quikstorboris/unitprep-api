//! Loads `client_ops.vendor_format` rows into
//! `unitprep_core::vendor_format::VendorFormat` — the one DB-touching
//! step between that table and both Group Prep's and dedup's
//! recognize/apply-mapping calls.
//!
//! A small repository module here, rather than an inline query in each
//! tool's own handler the way `client_ops.qms_tag`'s smaller catalog is
//! read (see this module's parent doc comment) — this table is read
//! from two unrelated tools, and duplicating the load-plus-row-shape
//! logic in both would recreate exactly the "two copies that quietly
//! drift apart" pattern this project's own review flagged elsewhere.
//!
//! `VendorFormatCache` + `start_retry_if_unloaded` exist because vendor rows
//! must NOT be queried per HTTP request: `AppState::db` is deliberately
//! lazy and never blocks startup on Postgres being reachable (see
//! `db.rs`), and both discovery's and dedup's own request handlers are
//! called directly in a large existing test suite against a pool that
//! never actually connects (`test_support::test_db_pool`) — those tests
//! were never supposed to need a database, and a per-request vendor
//! lookup would silently turn every one of them into a ~50ms failure
//! instead of the instant in-memory result they are today. Loading once
//! at startup keeps request handling itself synchronous and DB-free.
//!
//! **No periodic refresh, on purpose.** Nothing writes `vendor_format`
//! while the server runs: rows change through migrations, and a deploy
//! restarts the server, which reloads them. A timer would only wake a
//! scale-to-zero database (Neon) on a schedule to re-read a table that has
//! not changed -- a 4-hour timer here (three of them, with the file-meta
//! cache) meant about six compute wakes a day on an idle server. What is
//! left is a retry that runs only when the startup load failed (see
//! [`start_retry_if_unloaded`]). A future self-service "add a vendor" UI
//! should call [`refresh`] from its save handler so the new row shows up
//! at once, rather than reintroducing a timer.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use sqlx::PgPool;
use uuid::Uuid;

use unitprep_core::vendor_format::{ContentType, VendorFormat};

use crate::auth::begin_rls_transaction;

/// Shared, swappable snapshot of one content type's vendor list. Reads
/// are a synchronous lock + clone (see `apply_field_mapping`'s callers)
/// -- no `.await` anywhere on the request path.
pub type VendorFormatCache = Arc<RwLock<Vec<VendorFormat>>>;

/// A fixed, non-empty placeholder for `app.current_user_id` -- this read
/// happens at server startup and on a timer, outside any real HTTP
/// request, so there is no authenticated caller to attribute it to.
/// `vendor_format`'s SELECT policy only checks that the setting is
/// present and non-empty (see the registry migration), not that it names
/// a real user, so any well-formed placeholder satisfies it.
const SYSTEM_USER_ID: Uuid = Uuid::nil();

/// Best-effort initial load for `AppState` construction. Returns an
/// empty cache (logged, not propagated as a startup failure) if
/// Postgres isn't reachable yet -- consistent with `db.rs`'s own
/// "never block startup on the database" stance. [`start_retry_if_unloaded`]
/// fills it in once Postgres is reachable.
pub async fn initial_cache(db: &PgPool, content_type: ContentType) -> VendorFormatCache {
    let vendors = match load_vendor_formats(db, SYSTEM_USER_ID, &[], content_type).await {
        Ok(vendors) => vendors,
        Err(err) => {
            tracing::warn!(
                error = %err,
                content_type = content_type.as_db_str(),
                "Initial vendor-format load failed -- starting with an empty registry; the retry task will keep trying"
            );
            Vec::new()
        }
    };

    Arc::new(RwLock::new(vendors))
}

/// Re-reads `client_ops.vendor_format` for `content_type` into `cache`.
/// Used by the startup retry below and meant for a future vendor-editing
/// handler to call after it saves, so a new row is live without a restart.
pub(crate) async fn refresh(
    cache: &VendorFormatCache,
    db: &PgPool,
    content_type: ContentType,
) -> anyhow::Result<()> {
    let vendors = load_vendor_formats(db, SYSTEM_USER_ID, &[], content_type).await?;
    *cache.write() = vendors;
    Ok(())
}

/// If the startup load left `cache` empty (Postgres was unreachable at
/// boot), retries in the background with a growing delay until one load
/// succeeds, then stops. Does nothing when the cache loaded fine -- the
/// table is seeded by migrations, so a healthy startup never comes back
/// empty. Unlike the 4-hour timer this replaced, a healthy server never
/// touches the database from here again.
pub fn start_retry_if_unloaded(cache: VendorFormatCache, db: PgPool, content_type: ContentType) {
    if !cache.read().is_empty() {
        return;
    }

    spawn_retry_until_ok(
        content_type.as_db_str(),
        RETRY_FIRST_DELAY,
        RETRY_MAX_DELAY,
        move || {
            let cache = cache.clone();
            let db = db.clone();
            async move { refresh(&cache, &db, content_type).await }
        },
    );
}

/// Delay before the first retry after a failed startup load.
pub(crate) const RETRY_FIRST_DELAY: Duration = Duration::from_secs(30);

/// Longest the retry delay grows to (it doubles after each failure).
pub(crate) const RETRY_MAX_DELAY: Duration = Duration::from_secs(15 * 60);

/// Runs `attempt` after `first_delay`, again after twice that and so on
/// (capped at `max_delay`), until it returns `Ok`, then ends. Shared by
/// this module's cache and `vendor_file_meta`'s so the two cannot drift.
/// Returns the task handle so a test can await it.
pub(crate) fn spawn_retry_until_ok<F, Fut>(
    label: &'static str,
    first_delay: Duration,
    max_delay: Duration,
    mut attempt: F,
) -> tokio::task::JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<()>> + Send,
{
    tokio::spawn(async move {
        let mut delay = first_delay;
        loop {
            tokio::time::sleep(delay).await;
            match attempt().await {
                Ok(()) => {
                    tracing::info!(
                        content_type = label,
                        "Registry loaded on retry; retry task done"
                    );
                    return;
                }
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        content_type = label,
                        "Registry load still failing; keeping the empty snapshot and retrying"
                    );
                    delay = (delay * 2).min(max_delay);
                }
            }
        }
    })
}

#[derive(sqlx::FromRow)]
struct VendorFormatRow {
    name: String,
    signature_headers: Vec<String>,
    field_mapping: serde_json::Value,
    transform_key: Option<String>,
}

#[derive(serde::Deserialize)]
struct MappingEntry {
    target: String,
    source: String,
}

/// Loads every `client_ops.vendor_format` row for `content_type`,
/// ordered by `id` — which is also detection-priority order, so a
/// vendor whose signature is a strict superset of another's (e.g. real
/// Storage Commander exports also satisfy plain QSX's own signature)
/// must be inserted before the one it would otherwise shadow. See the
/// registry migration's own seed data and comments for the concrete
/// case this matters for.
///
/// Goes through `begin_rls_transaction`, same as every other client_ops
/// read (see `api::client_ops_qms_tags`) — `vendor_format`'s own SELECT
/// policy requires `app.current_user_id` to be set, so a caller that
/// queried the pool directly wouldn't get an error, just zero rows back,
/// which is a far worse failure mode (looks exactly like "no vendors
/// recognized this file" instead of "this call is wired wrong").
pub async fn load_vendor_formats(
    db: &PgPool,
    user_id: Uuid,
    role_keys: &[String],
    content_type: ContentType,
) -> anyhow::Result<Vec<VendorFormat>> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;

    let rows: Vec<VendorFormatRow> = sqlx::query_as(
        "SELECT name, signature_headers, field_mapping, transform_key
         FROM client_ops.vendor_format
         WHERE content_type = $1
         ORDER BY id",
    )
    .bind(content_type.as_db_str())
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await?;

    rows.into_iter()
        .map(|row| {
            let mapping: Vec<MappingEntry> = serde_json::from_value(row.field_mapping)?;
            Ok(VendorFormat {
                name: row.name,
                content_type,
                signature_headers: row.signature_headers,
                field_mapping: mapping
                    .into_iter()
                    .map(|entry| (entry.target, entry.source))
                    .collect(),
                transform_key: row.transform_key,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::time::Instant;

    use super::*;

    /// Runs the retry helper against an attempt that fails `failures` times
    /// and then succeeds, on a paused clock, returning how many attempts it
    /// made and how much (virtual) time passed. First delay 10 s, cap 25 s.
    async fn run_with_failures(failures: usize) -> (usize, Duration) {
        let attempts = Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let started = Instant::now();

        spawn_retry_until_ok(
            "test",
            Duration::from_secs(10),
            Duration::from_secs(25),
            move || {
                let counter = counter.clone();
                async move {
                    let attempt = counter.fetch_add(1, Ordering::SeqCst);
                    if attempt < failures {
                        Err(anyhow::anyhow!("database not reachable"))
                    } else {
                        Ok(())
                    }
                }
            },
        )
        .await
        .unwrap();

        (attempts.load(Ordering::SeqCst), started.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn the_retry_waits_the_first_delay_and_stops_after_one_success() {
        let (attempts, elapsed) = run_with_failures(0).await;

        assert_eq!(attempts, 1);
        assert_eq!(elapsed, Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn the_retry_delay_doubles_after_each_failure_and_caps() {
        // Delays: 10 s, then 20 s, then 40 s capped to 25 s, then 25 s.
        let (attempts, elapsed) = run_with_failures(3).await;

        assert_eq!(attempts, 4);
        assert_eq!(elapsed, Duration::from_secs(10 + 20 + 25 + 25));
    }

    /// Needs the local `test-db`: `refresh` must fill an empty cache from
    /// the seeded registry, through the app role's RLS like production.
    #[tokio::test]
    #[ignore = "needs the local test-db"]
    async fn refresh_fills_an_empty_cache_from_the_seeded_registry() {
        let _ = dotenvy::from_filename(".env.local");
        let db = crate::db::connect_test();
        let cache: VendorFormatCache = Arc::new(RwLock::new(Vec::new()));

        refresh(&cache, &db, ContentType::Tenants).await.unwrap();

        assert!(!cache.read().is_empty());
    }
}
