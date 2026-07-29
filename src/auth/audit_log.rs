//! Append-only audit-event recording.
//!
//! The build plan calls for audit capture wired in from day one rather
//! than switched on later, so there is no gap in the record. This is the
//! one place that writes `auth.auth_audit_logs`.
//!
//! ## Never use `INSERT ... RETURNING` here
//!
//! `RETURNING` requires the newly inserted row to be *readable*, so it is
//! evaluated against `auth_audit_logs_select_admin_only`. The insert
//! therefore succeeds or fails depending on whether `app.current_user_role`
//! happens to be `'admin'` at the time:
//!
//! | statement | identity context | result |
//! |---|---|---|
//! | `INSERT ... RETURNING id` | none | **ERROR** |
//! | `INSERT ... RETURNING id` | `role = 'admin'` | ok |
//! | `INSERT ...` (no RETURNING) | none | ok |
//!
//! The error is `new row violates row-level security policy`, which points
//! at the wrong thing entirely -- the `WITH CHECK (true)` insert policy
//! passed fine; it is reading the returned row that fails.
//!
//! What makes this worth a module-level warning is that the failure is
//! *selective*: logging would work for admin-context events and break on
//! precisely the events that have no identity context -- failed logins --
//! which are the ones the audit trail exists for. Verified all four
//! combinations as `app_service` against the dev branch 2026-07-29.
//!
//! So: no `RETURNING`, and no GUC context is set up before writing. The
//! insert policy is unconditional by design specifically so that an event
//! with no known actor still lands.

use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Event types written so far. A plain `&'static str` rather than an enum
/// mapped to a Postgres type, matching the schema's deliberate choice to
/// keep `event_type` as `text`: this set grows new categories often, and
/// `ALTER TYPE ... ADD VALUE` on a frequently-extended column is friction
/// for no benefit. Kept as constants so a typo is a compile error at the
/// call site rather than a silently unqueryable row.
pub mod event {
    pub const LOGIN_SUCCEEDED: &str = "login_succeeded";
    pub const LOGIN_FAILED: &str = "login_failed";
    pub const PASSKEY_REGISTERED: &str = "passkey_registered";
}

/// Records one audit event.
///
/// Deliberately infallible from the caller's point of view: a failure to
/// write the audit row is logged and swallowed rather than propagated.
/// Returning an error here would tempt callers into failing the request
/// they were auditing -- which would turn a logging outage into an
/// authentication outage, and hand anyone who could break audit writes a
/// denial-of-service on login. The audit trail is important, but it is
/// not more important than the operation it describes.
///
/// `actor_user_id` is `None` for events with no established identity --
/// a login attempt against an unknown address, for instance.
pub async fn record(
    db: &PgPool,
    event_type: &str,
    actor_user_id: Option<Uuid>,
    user_agent: Option<&str>,
    metadata: Value,
) {
    // No RETURNING -- see the module doc above.
    let result = sqlx::query(
        "INSERT INTO auth.auth_audit_logs (event_type, actor_user_id, user_agent, metadata)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(event_type)
    .bind(actor_user_id)
    .bind(user_agent)
    .bind(&metadata)
    .execute(db)
    .await;

    if let Err(err) = result {
        // `error`, not `warn`: a missing audit row is a real gap in a
        // record that is supposed to be complete, even though the request
        // itself is allowed to continue.
        tracing::error!(
            error = %err,
            event_type,
            actor_user_id = ?actor_user_id,
            "failed to write audit log event"
        );
    }
}
