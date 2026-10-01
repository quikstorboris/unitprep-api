//! File-level metadata for the dedup folder scan: for every registered
//! tenant file format, which PMS it belongs to, what the report is
//! called, whether it can run on its own, which alternative to
//! pre-select, and the guidance text shown to the user. Lives in the
//! same `client_ops.vendor_format` rows as the recognition data (see the
//! `add_dedup_file_metadata` migration) but is loaded separately into its
//! own snapshot, because `unitprep_core::vendor_format::VendorFormat` is
//! persisted inside Group Prep's durable sessions: adding fields to it
//! would change that serialized shape for in-flight sessions.
//!
//! Same caching stance as `vendor_format`: an in-memory snapshot read
//! synchronously on the request path (never a per-request DB call),
//! refreshed on a timer.

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use sqlx::PgPool;
use uuid::Uuid;

use unitprep_core::vendor_format::ContentType;
use unitprep_dedup::file_selection::{FileFormatMeta, FileRole};

use crate::auth::begin_rls_transaction;

pub type FileMetaCache = Arc<RwLock<Vec<FileFormatMeta>>>;

/// See `vendor_format::SYSTEM_USER_ID` -- the same startup/timer read
/// with no authenticated caller, satisfying the same SELECT policy.
const SYSTEM_USER_ID: Uuid = Uuid::nil();

/// Matches `vendor_format::start_refresh_task`'s interval, for the same
/// Neon autosuspend reason documented there.
const REFRESH_INTERVAL: Duration = Duration::from_secs(4 * 60 * 60);

/// Best-effort initial load; an unreachable database yields an empty
/// snapshot (the refresh task retries) instead of failing startup.
pub async fn initial_cache(db: &PgPool, content_type: ContentType) -> FileMetaCache {
    let metas = match load_file_meta(db, SYSTEM_USER_ID, &[], content_type).await {
        Ok(metas) => metas,
        Err(err) => {
            tracing::warn!(
                error = %err,
                content_type = content_type.as_db_str(),
                "Initial vendor file-metadata load failed -- starting empty; the refresh task will retry"
            );
            Vec::new()
        }
    };

    Arc::new(RwLock::new(metas))
}

pub fn start_refresh_task(cache: FileMetaCache, db: PgPool, content_type: ContentType) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(REFRESH_INTERVAL);

        loop {
            interval.tick().await;

            match load_file_meta(&db, SYSTEM_USER_ID, &[], content_type).await {
                Ok(metas) => *cache.write() = metas,
                Err(err) => tracing::error!(
                    error = %err,
                    content_type = content_type.as_db_str(),
                    "Vendor file-metadata refresh failed; keeping the previous snapshot"
                ),
            }
        }
    });
}

#[derive(sqlx::FromRow)]
struct FileMetaRow {
    name: String,
    pms: String,
    report_name: String,
    file_role: String,
    selection_priority: i32,
    guidance: String,
}

/// Every format's file metadata for `content_type`, in `id` order (the
/// same order detection uses). Goes through `begin_rls_transaction` for
/// the reason `vendor_format::load_vendor_formats` documents: a bare
/// pool read would return zero rows, not an error.
pub async fn load_file_meta(
    db: &PgPool,
    user_id: Uuid,
    role_keys: &[String],
    content_type: ContentType,
) -> anyhow::Result<Vec<FileFormatMeta>> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;

    let rows: Vec<FileMetaRow> = sqlx::query_as(
        "SELECT name, pms, COALESCE(report_name, name) AS report_name, file_role,
                selection_priority, COALESCE(guidance, '') AS guidance
         FROM client_ops.vendor_format
         WHERE content_type = $1
         ORDER BY id",
    )
    .bind(content_type.as_db_str())
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(rows
        .into_iter()
        .map(|row| FileFormatMeta {
            name: row.name,
            pms: row.pms,
            report_name: row.report_name,
            role: FileRole::from_db_str(&row.file_role),
            selection_priority: row.selection_priority,
            guidance: row.guidance,
        })
        .collect())
}

#[cfg(test)]
#[path = "vendor_file_meta_tests.rs"]
mod tests;
