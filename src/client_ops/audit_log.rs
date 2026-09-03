//! Append-only operations trail for the client-ops domain -- distinct
//! from `auth::audit_log`, which is the *security* audit trail (logins,
//! role changes, authorization failures). This one exists so that
//! mutations to client-ops data (today: edits to the hand-maintained QMS
//! tag catalog; later: client QMS credential adds/revokes, and whatever
//! else this domain grows) leave a record, without mixing "who changed
//! this business data" into the same table as "who is trying to break
//! in" -- the same access-boundary split already locked between Admin's
//! oversight audit and client-ops's own data (see the vault's Grok
//! response on this, if this comment is ever the only trace left of the
//! reasoning: a single audit table filtered by column was rejected for
//! exactly this reason).
//!
//! Reuses `auth::audit_log::Change` for the before/after JSONB shape --
//! that type carries no auth-specific meaning, just "a value transition
//! or nothing." Does not reuse `Subjects`: that type's `target` is a
//! `Uuid` (another user), which does not fit an entity identified by a
//! string key like `tag_key`. Every event here has a real actor by
//! construction (only an authenticated, permitted caller ever reaches the
//! code path that writes one) -- there is no anonymous-event case the way
//! a failed login is one, so `actor_user_id` is a plain `Uuid`, not
//! `Option<Uuid>`.

use serde_json::Value;
use sqlx::types::ipnetwork::IpNetwork;
use sqlx::PgPool;
use uuid::Uuid;

pub use crate::auth::audit_log::Change;

/// Event types written so far. Same reasoning as `auth::audit_log::event`
/// for keeping this a `text` column with `&'static str` constants rather
/// than a Postgres enum: this list is expected to grow as client-ops
/// grows, and a typo at the call site should be a compile error, not a
/// silently unqueryable row.
pub mod event {
    pub const QMS_TAG_CREATED: &str = "qms_tag_created";
    pub const QMS_TAG_UPDATED: &str = "qms_tag_updated";
    pub const QMS_TAG_DEACTIVATED: &str = "qms_tag_deactivated";
    pub const QMS_TAG_REACTIVATED: &str = "qms_tag_reactivated";

    /// A client + its facilities were imported from Process Street via
    /// the "Add to OO" confirmation screen (`api::clients_create`).
    pub const CLIENT_CREATED: &str = "client_created";
    /// A dedup-tool run was exported (`api::dedup::export`/`export_to_dropbox`).
    pub const DEDUP_COMPLETED: &str = "dedup_completed";
    /// A Unit Group run was exported (`api::export::export`).
    pub const UNIT_GROUP_COMPLETED: &str = "unit_group_completed";
    /// A Process Street sync (scheduled or the manual "Sync Now"/scoped
    /// Re-sync trigger) finished successfully -- see `clients::sync`.
    pub const SYNC_COMPLETED: &str = "sync_completed";
    /// A Process Street sync failed partway through -- e.g. the PS API
    /// was unreachable or returned an error. Recorded so an unresponsive
    /// PS incident shows up in the same trail as every other activity,
    /// not just in server logs.
    pub const SYNC_FAILED: &str = "sync_failed";
    /// A facility's Merchant Account run was manually linked via the
    /// Elavon tab's "link" action (`api::clients_elavon`) -- distinct
    /// from `CLIENT_CREATED`'s own automatic correlation, since this is
    /// a deliberate, one-at-a-time human action confirming a specific
    /// run id.
    pub const MERCHANT_ACCOUNT_LINKED: &str = "merchant_account_linked";

    /// Every event type this module writes, for the Activity Logs admin
    /// filter dropdown -- mirrors `auth::audit_log::event::ALL`'s own
    /// reasoning: straight from this list, so the frontend can never
    /// drift from what this backend actually writes.
    pub const ALL: &[&str] = &[
        QMS_TAG_CREATED,
        QMS_TAG_UPDATED,
        QMS_TAG_DEACTIVATED,
        QMS_TAG_REACTIVATED,
        CLIENT_CREATED,
        DEDUP_COMPLETED,
        UNIT_GROUP_COMPLETED,
        SYNC_COMPLETED,
        SYNC_FAILED,
        MERCHANT_ACCOUNT_LINKED,
    ];
}

/// Records one client-ops audit event. Infallible from the caller's point
/// of view, same reasoning as `auth::audit_log::record`: a failure to
/// write this row is logged and swallowed rather than propagated, so a
/// logging hiccup can never turn into a failed tag edit.
///
/// No `RETURNING` here either -- not because the same RLS trap
/// necessarily applies (every actor here already satisfies the SELECT
/// policy for their own write, since only the three client-ops roles
/// reach this code path at all), but because there is no benefit to
/// reading the row back, and matching the established no-`RETURNING`
/// convention costs nothing.
#[allow(clippy::too_many_arguments)]
pub async fn record(
    db: &PgPool,
    event_type: &str,
    actor_user_id: Uuid,
    entity_type: &str,
    entity_id: Option<&str>,
    change: Change,
    user_agent: Option<&str>,
    ip_address: Option<IpNetwork>,
    metadata: Value,
) {
    let result = sqlx::query(
        "INSERT INTO client_ops.audit_log
             (event_type, actor_user_id, entity_type, entity_id,
              before_state, after_state, user_agent, ip_address, metadata)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(event_type)
    .bind(actor_user_id)
    .bind(entity_type)
    .bind(entity_id)
    .bind(&change.before)
    .bind(&change.after)
    .bind(user_agent)
    .bind(ip_address)
    .bind(&metadata)
    .execute(db)
    .await;

    if let Err(err) = result {
        tracing::error!(
            error = %err,
            event_type,
            actor_user_id = %actor_user_id,
            entity_type,
            entity_id,
            "failed to write client-ops audit log event"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::event::ALL;
    use std::collections::HashSet;

    // Same test, same reasoning, as `auth::audit_log::event`'s own copy
    // -- this list had no coverage at all before 2026-09-02 added five
    // more constants to it in one pass, exactly the kind of change where
    // a copy-paste duplicate is easy to introduce unnoticed.
    #[test]
    fn event_types_has_no_duplicates() {
        let unique: HashSet<&str> = ALL.iter().copied().collect();
        assert_eq!(unique.len(), ALL.len(), "ALL contains a duplicate entry");
    }
}
