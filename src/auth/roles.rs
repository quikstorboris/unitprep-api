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

/// Counts active admins other than `excluded_user_id`, for a caller about
/// to revoke/deactivate one and refuse doing so if it would zero out
/// admins entirely.
///
/// Locks the `admin` role row itself before counting -- a `FOR UPDATE` on
/// the count query alone is not enough. Two concurrent callers each
/// acting on a *different* admin only ever lock (or don't lock) rows
/// belonging to the *other* admin, so their row locks never conflict:
/// both can see "one other admin remains" from their own still-isolated
/// snapshot and both commit, leaving zero. Locking one row shared by both
/// transactions -- the `admin` role itself -- is what actually forces the
/// second caller to wait for the first to commit (or roll back) before it
/// re-counts, so the check runs against up-to-date reality instead of two
/// transactions' mutually-blind snapshots.
pub async fn remaining_active_admins_excluding(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    excluded_user_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query("SELECT id FROM auth.roles WHERE key = 'admin' FOR UPDATE")
        .fetch_one(&mut **tx)
        .await?;

    sqlx::query_scalar(
        "SELECT count(*) FROM auth.users u
           JOIN auth.user_roles ur ON ur.user_id = u.id
           JOIN auth.roles r ON r.id = ur.role_id
          WHERE r.key = 'admin'
            AND u.status = 'active'::auth.user_status
            AND u.deleted_at IS NULL
            AND u.id != $1",
    )
    .bind(excluded_user_id)
    .fetch_one(&mut **tx)
    .await
}
