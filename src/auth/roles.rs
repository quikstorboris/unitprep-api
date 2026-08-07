//! Shared helpers for resolving role keys against `auth.roles`/
//! `auth.user_roles` -- used wherever a client-supplied role string needs
//! validating (invite creation, granting a role) or a user's full current
//! role set needs reading back (audit before/after state, last-admin-style
//! guards). Deliberately plain queries against tables any authenticated
//! caller can already read under RLS (`roles_select_authenticated`,
//! `user_roles_select_own_or_admin`), not SECURITY DEFINER functions --
//! unlike session resolution, everything here runs inside an
//! already-established `begin_rls_transaction`, so there is no pre-auth
//! problem to work around.

use uuid::Uuid;

/// Looks up a role's id by its key. Returns `None` for an unknown key
/// rather than erroring, so callers can turn that into a clean 400 naming
/// the bad value instead of a raw Postgres enum-cast-style error.
pub async fn resolve_role_id(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    role_key: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar("SELECT id FROM auth.roles WHERE key = $1")
        .bind(role_key)
        .fetch_optional(&mut **tx)
        .await
}

/// The full, current set of role keys a user holds, sorted -- the same
/// shape `resolve_session` resolves at login, but callable mid-request,
/// e.g. to build an audit event's before/after state around a grant or
/// revoke. Always returns exactly one row (an aggregate with no GROUP BY
/// does, even over zero matches), so `Option<Vec<String>>` collapses to
/// an empty `Vec` rather than needing a separate "no roles" branch.
pub async fn role_keys_for_user(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> Result<Vec<String>, sqlx::Error> {
    let keys: Option<Vec<String>> = sqlx::query_scalar(
        "SELECT array_agg(r.key ORDER BY r.key)
           FROM auth.user_roles ur
           JOIN auth.roles r ON r.id = ur.role_id
          WHERE ur.user_id = $1",
    )
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await?;

    Ok(keys.unwrap_or_default())
}
