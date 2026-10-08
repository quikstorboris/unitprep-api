//! The one audit row an admin's edit of an org-wide integration setting
//! leaves behind (`auth::audit_log::event::INTEGRATION_SETTINGS_UPDATED`).
//!
//! Shared by the Dropbox, Process Street and Process Street task-role
//! settings handlers so all three write the same shape. Callers build
//! `details` themselves and must put NO secret value in it -- only that a
//! secret was replaced (`"api_key_replaced": true`).

use serde_json::{json, Value};
use sqlx::types::ipnetwork::IpNetwork;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::audit_log::{self, Change, Subjects};

/// Records the change after it has committed. Infallible from the
/// caller's point of view, like every audit write: a failure to write the
/// row is logged and swallowed, never turned into a failed settings save.
pub(crate) async fn record_settings_updated(
    db: &PgPool,
    actor: Uuid,
    user_agent: Option<&str>,
    ip_address: Option<IpNetwork>,
    integration: &str,
    details: Value,
) {
    audit_log::record(
        db,
        audit_log::event::INTEGRATION_SETTINGS_UPDATED,
        Subjects::by(actor),
        user_agent,
        ip_address,
        Change::none(),
        json!({ "integration": integration, "details": details }),
    )
    .await;
}
