//! Writes the verified credential and, on the invite path, consumes the invite in the same transaction.

use crate::api::AppState;
use crate::auth::{begin_owner_rls_transaction, RegisteredCredential};
use uuid::Uuid;

/// Writes the verified credential under the target user's own identity
/// and, on the invite path, consumes the invite **in the same
/// transaction**.
///
/// Returns `Ok(false)` when an invite was supplied but was no longer
/// consumable -- expired between `begin` and `finish`, or used by a
/// concurrent attempt. In that case the transaction is rolled back and
/// nothing at all is written.
///
/// ## Why one transaction, and not two statements in a tidy order
///
/// `consume_invite` does two things: it marks the invite used and flips
/// the user from `invited` to `active`. Pairing that with the credential
/// insert non-atomically leaves a stranded account whichever order is
/// chosen, and **both stranded states are unrecoverable** with the
/// current tooling, because `bootstrap-admin --reissue-invite` refuses an
/// account that is not `invited` *and* refuses one that already holds a
/// credential:
///
/// | if this failed | leaves | `--reissue-invite` |
/// |---|---|---|
/// | insert, after consume | `active`, no credential | refuses: not `invited` |
/// | consume, after insert | `invited`, has credential | refuses: has a credential |
///
/// Wrapping both makes the only reachable outcomes "enrolled and active"
/// or "untouched and retryable". The user-visible payoff is that
/// cancelling the Windows Hello prompt costs nothing.
///
/// Uses the owner-only GUC helper rather than `begin_rls_transaction`:
/// `webauthn_credentials`' RLS policy consults only
/// `app.current_user_id`, and the invite path has no established role to
/// assert anyway -- so setting the role GUC here would hand this write
/// admin visibility it has no use for. `consume_invite` is unaffected by
/// either GUC, being `SECURITY DEFINER`; that is also what lets it update
/// `auth.users.status`, a column `app_service` deliberately cannot write.
pub(super) async fn enrol_credential(
    state: &AppState,
    user_id: Uuid,
    registered: &RegisteredCredential,
    nickname: Option<&str>,
    invite_token_hash: Option<&[u8]>,
) -> Result<bool, sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(&state.db, user_id).await?;

    // device_bound is written explicitly rather than left to the column's
    // `DEFAULT true`. Relying on the default meant every row claimed the
    // credential could not leave its hardware, including synced passkeys
    // where that is simply false -- a fabricated value, which is worse
    // than a null one because nothing about it looks wrong. Found when the
    // first real Windows Hello credential turned out to be
    // backup-eligible while its row said otherwise.
    sqlx::query(
        "INSERT INTO auth.webauthn_credentials
             (user_id, credential_id, passkey_data, nickname, device_bound)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(user_id)
    .bind(&registered.credential_id)
    .bind(&registered.passkey_data)
    .bind(nickname)
    .bind(registered.device_bound)
    .execute(&mut *tx)
    .await?;

    if let Some(token_hash) = invite_token_hash {
        let activated: Option<Uuid> = sqlx::query_scalar("SELECT auth.consume_invite($1)")
            .bind(token_hash)
            .fetch_one(&mut *tx)
            .await?;

        // `consume_invite` returns NULL when its guarded UPDATE matched
        // nothing -- the invite is used or expired. Roll back rather than
        // commit a credential for an account that stays `invited`, since
        // that is one of the two unrecoverable states above.
        let Some(activated) = activated else {
            tx.rollback().await?;
            return Ok(false);
        };

        // Defensive, and cheap. The token resolved to this user at
        // `begin`, so a different id here would mean the invite moved
        // between requests -- impossible by construction today, but the
        // cost of being wrong is a credential written onto the wrong
        // account, so it is checked rather than assumed.
        if activated != user_id {
            tx.rollback().await?;
            tracing::error!(
                expected_user_id = %user_id,
                activated_user_id = %activated,
                "invite consumption activated a different user than the ceremony resolved"
            );
            return Ok(false);
        }
    }

    tx.commit().await?;
    Ok(true)
}
