//! Admin-configurable policy for which self-service actions require a
//! fresh TOTP step-up before proceeding -- backed by
//! `auth.auth_configuration.step_up_actions`, a JSONB array of action
//! names, rather than a hardcoded check per action. An admin can add or
//! remove a gated action by editing this row, not by a code change and a
//! deploy.
//!
//! Distinct from the Phase II login-anomaly session flag
//! (`auth.sessions.requires_step_up`, see auth_login.rs) -- that gates an
//! entire session immediately after an anomalous login; this gates one
//! specific action for a session that is otherwise perfectly ordinary.
//! The two mechanisms don't interact.

use sqlx::PgPool;
use uuid::Uuid;

use super::begin_owner_rls_transaction;

/// Adding a new passkey to an account that already has one -- the first,
/// and so far only, action gated this way. Seeded into
/// `auth.auth_configuration.step_up_actions` by the migration that added
/// this policy check, so wiring it up didn't silently disable protection
/// that already existed unconditionally.
pub const ADD_PASSKEY: &str = "add_passkey";

/// Whether `action` currently requires a fresh step-up before proceeding.
///
/// Read under the caller's own identity via `begin_owner_rls_transaction`
/// -- deliberately not admin-only, since an ordinary user needs to know
/// whether *their own* action is gated, not just an admin reviewing
/// policy. See the RLS policy `auth_configuration_select_any_authenticated`,
/// added alongside this for exactly that reason: `auth_configuration` was
/// admin-only-readable before, which would have made this check
/// impossible for a non-admin caller.
pub async fn action_requires_step_up(
    db: &PgPool,
    user_id: Uuid,
    action: &str,
) -> Result<bool, sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(db, user_id).await?;

    let required: bool =
        sqlx::query_scalar("SELECT step_up_actions ? $1 FROM auth.auth_configuration WHERE id = 1")
            .bind(action)
            .fetch_one(&mut *tx)
            .await?;

    tx.commit().await?;
    Ok(required)
}
