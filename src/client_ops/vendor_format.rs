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
//! `VendorFormatCache` + `start_refresh_task` exist because vendor rows
//! must NOT be queried per HTTP request: `AppState::db` is deliberately
//! lazy and never blocks startup on Postgres being reachable (see
//! `db.rs`), and both discovery's and dedup's own request handlers are
//! called directly in a large existing test suite against a pool that
//! never actually connects (`test_support::test_db_pool`) — those tests
//! were never supposed to need a database, and a per-request vendor
//! lookup would silently turn every one of them into a ~50ms failure
//! instead of the instant in-memory result they are today. Loading once
//! at startup and refreshing on a timer (same shape as
//! `InMemorySessionStore::start_cleanup_task`) keeps request handling
//! itself synchronous and DB-free, while still picking up a
//! self-service-added vendor row within one refresh interval without a
//! restart.

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
/// "never block startup on the database" stance. `start_refresh_task`
/// will fill it in on its first tick once Postgres is reachable.
pub async fn initial_cache(db: &PgPool, content_type: ContentType) -> VendorFormatCache {
    let vendors = match load_vendor_formats(db, SYSTEM_USER_ID, &[], content_type).await {
        Ok(vendors) => vendors,
        Err(err) => {
            tracing::warn!(
                error = %err,
                content_type = content_type.as_db_str(),
                "Initial vendor-format load failed -- starting with an empty registry; the refresh task will retry"
            );
            Vec::new()
        }
    };

    Arc::new(RwLock::new(vendors))
}

/// Refreshes `cache` from `client_ops.vendor_format` every 5 minutes --
/// long enough that this is never a meaningful load on Postgres, short
/// enough that a vendor added through the (future) self-service UI, or a
/// Postgres connection that wasn't up yet at boot, shows up without a
/// restart. Mirrors `InMemorySessionStore::start_cleanup_task`'s shape:
/// spawned once at startup, loops forever, one failed tick is logged and
/// skipped rather than ending the task.
pub fn start_refresh_task(cache: VendorFormatCache, db: PgPool, content_type: ContentType) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300));

        loop {
            interval.tick().await;

            match load_vendor_formats(&db, SYSTEM_USER_ID, &[], content_type).await {
                Ok(vendors) => *cache.write() = vendors,
                Err(err) => tracing::error!(
                    error = %err,
                    content_type = content_type.as_db_str(),
                    "Vendor-format refresh tick failed; keeping the previous snapshot"
                ),
            }
        }
    });
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
