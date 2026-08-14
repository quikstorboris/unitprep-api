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
///
/// Must be called from a transaction whose RLS context (see
/// `begin_rls_transaction`) actually holds the `admin` role -- not just
/// any authenticated caller. `SELECT ... FOR UPDATE` is governed by
/// `auth.roles`' SELECT policy (any authenticated caller) *and* its
/// UPDATE policy (`roles_update_admin_only`, admin-only), since acquiring
/// a lock is treated as a preliminary step toward a possible update; a
/// non-admin's `FOR UPDATE` silently sees zero rows here (a hard
/// `RowNotFound`, not a graceful refusal) even though a plain `SELECT`
/// with the same GUCs would return the row fine. In practice this is
/// always satisfied -- the only two callers (`revoke_role`/
/// `deactivate_user`) already require an admin caller to reach this far
/// -- but it is exactly the kind of thing that looks like "any
/// authenticated caller" from this function's signature alone.
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// Real concurrency test for the last-active-admin race fix: proves
    /// the `FOR UPDATE` lock on the shared `admin` role row genuinely
    /// serializes two concurrent callers, rather than trusting the
    /// reasoning in this function's own doc comment unverified.
    ///
    /// Needs a real, reachable Postgres (`DATABASE_URL` from `.env.local`)
    /// -- `#[ignore]`d for the same reason `query_session`'s real-schema
    /// test is: this crate's fast offline suite stays fast and offline by
    /// default. Run explicitly with
    /// `cargo test -- --ignored remaining_active_admins_excluding`.
    ///
    /// Two genuinely separate connections (not two `Transaction`s sharing
    /// one) each start a transaction and call
    /// `remaining_active_admins_excluding`. The first holds its lock for
    /// a deliberate delay before committing; the second call is timed --
    /// if the lock is doing its job, the second call cannot return until
    /// the first transaction ends, so the elapsed time must be at least
    /// as long as the first's hold duration. The original, race-prone
    /// shape (no `FOR UPDATE` at all) would let both calls return almost
    /// immediately, failing this assertion -- this test would have
    /// caught the race this function exists to close.
    ///
    /// Uses `begin_rls_transaction` rather than a bare `db.begin()` --
    /// `auth.roles` is RLS-protected (`roles_select_authenticated`
    /// requires `app.current_user_id` to be set to *something*, any
    /// authenticated caller), and every real call site already runs
    /// inside one of these. A bare connection sees zero rows under RLS,
    /// not an error, which would have made this test fail confusingly
    /// on `RowNotFound` instead of proving anything about the lock.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres -- see doc comment"]
    async fn remaining_active_admins_excluding_serializes_concurrent_callers() {
        let _ = dotenvy::from_filename(".env.local");
        let db = crate::db::connect().expect("DATABASE_URL must be set -- see .env.local");

        const HOLD: Duration = Duration::from_millis(400);
        // FOR UPDATE on auth.roles is governed by its SELECT policy AND
        // its admin-only UPDATE policy at once (see this function's own
        // doc comment) -- an empty role set here would silently see zero
        // rows instead of proving anything about the lock.
        let admin_role_keys = [String::from("admin")];

        let (lock_acquired_tx, lock_acquired_rx) = tokio::sync::oneshot::channel();

        let first = {
            let db = db.clone();
            let admin_role_keys = admin_role_keys.clone();
            tokio::spawn(async move {
                let mut tx =
                    crate::auth::begin_rls_transaction(&db, Uuid::new_v4(), &admin_role_keys)
                        .await
                        .expect("first transaction should begin");
                remaining_active_admins_excluding(&mut tx, Uuid::new_v4())
                    .await
                    .expect("first call should succeed");
                let _ = lock_acquired_tx.send(());
                tokio::time::sleep(HOLD).await;
                tx.commit().await.expect("first transaction should commit");
            })
        };

        lock_acquired_rx
            .await
            .expect("first task should signal after acquiring the lock");

        let started = Instant::now();
        let mut second_tx =
            crate::auth::begin_rls_transaction(&db, Uuid::new_v4(), &admin_role_keys)
                .await
                .expect("second transaction should begin");
        remaining_active_admins_excluding(&mut second_tx, Uuid::new_v4())
            .await
            .expect("second call should succeed once the lock is free");
        second_tx
            .commit()
            .await
            .expect("second transaction should commit");
        let elapsed = started.elapsed();

        first.await.expect("first task should not panic");

        assert!(
            elapsed >= HOLD,
            "the second caller returned after only {elapsed:?}, before the first \
             transaction's {HOLD:?} hold ended -- the shared-role-row lock is not \
             actually serializing concurrent callers"
        );
    }
}
