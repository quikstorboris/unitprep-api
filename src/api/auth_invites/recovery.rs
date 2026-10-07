//! Admin account recovery: revokes and reissues every credential on the target account.

use super::invite::CreateInviteResponse;
use crate::api::{bad_request, conflict, internal_error, ApiErrorBody, AppState};
use crate::auth::{audit_log, begin_rls_transaction, generate_token, AuthenticatedUser};
use crate::bootstrap::invite_hours;
use axum::extract::{ConnectInfo, Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::net::SocketAddr;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct RecoverAccountRequest {
    pub email: String,
}

/// Unlike `conflict` above, this is a genuine 404 -- there really is no
/// account behind the address, which an authenticated admin is entitled
/// to be told plainly, same reasoning `conflict` already applies to every
/// other refusal on this endpoint.
pub(super) fn account_not_found(email: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ApiErrorBody {
            error: "account_not_found",
            message: format!("No account found for {email}."),
        }),
    )
        .into_response()
}

/// Revokes every existing access path on an already-active account and
/// issues a fresh invite in its place -- the admin-mediated recovery
/// workflow for someone who has lost their only passkey (see
/// AUTHENTICATION.md's "Losing your device" section). Deliberately its
/// own endpoint rather than a flag on `create_invite`: the two operations
/// have very different blast radii if triggered by accident, and a
/// separate route makes the admin's intent unambiguous at the point of
/// the request rather than resting on a boolean default.
pub async fn recover_account(
    State(state): State<AppState>,
    admin: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<RecoverAccountRequest>,
) -> Response {
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    // Redundant with the RLS policy by design -- see create_invite above.
    if let Err(response) = admin
        .require_permission(
            &state.db,
            "users.manage",
            "recover_account",
            user_agent,
            ip_address,
        )
        .await
    {
        return response;
    }

    let email = request.email.trim().to_ascii_lowercase();

    if email.is_empty() || !email.contains('@') || email.split_whitespace().count() > 1 {
        return bad_request("invalid_email", "A valid email address is required.".into());
    }

    let (raw_token, token_hash) = generate_token();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(invite_hours());

    let outcome = recover_account_tx(&state, &admin, &email, &token_hash, expires_at).await;

    let (user_id, prior_status) = match outcome {
        Ok(RecoveryOutcome::Recovered {
            user_id,
            prior_status,
        }) => (user_id, prior_status),

        Ok(RecoveryOutcome::Refused {
            user_id,
            reason,
            message,
        }) => {
            // Only a refusal naming a real account is worth a permanent
            // row -- "the admin mistyped an email" is not a
            // security-relevant event, the same reasoning that keeps
            // create_invite's own input-validation failures unaudited.
            if let Some(target_user_id) = user_id {
                audit_log::record(
                    &state.db,
                    audit_log::event::INVITE_REFUSED,
                    audit_log::Subjects::by(admin.user_id).about(target_user_id),
                    user_agent,
                    ip_address,
                    audit_log::Change::none(),
                    serde_json::json!({ "reason": reason, "action": "recovery" }),
                )
                .await;
            }

            tracing::info!(
                admin_user_id = %admin.user_id,
                target_user_id = ?user_id,
                reason,
                "account recovery refused"
            );

            return match user_id {
                Some(_) => conflict("invite_not_applicable", message),
                None => account_not_found(&email),
            };
        }

        Err(err) => {
            tracing::error!(
                error = %err,
                admin_user_id = %admin.user_id,
                "failed to recover an account"
            );
            return internal_error("Could not recover this account");
        }
    };

    audit_log::record(
        &state.db,
        audit_log::event::ACCOUNT_RECOVERY_INITIATED,
        audit_log::Subjects::by(admin.user_id).about(user_id),
        user_agent,
        ip_address,
        // The account's status cycles active -> deactivated -> invited
        // inside recover_account_tx; the net transition an operator cares
        // about is "was active, is now invited" -- the intermediate
        // deactivated step is real (it is what triggers the
        // revoke-every-access-path behaviour) but not a separate fact
        // worth its own before/after pair.
        audit_log::Change::from_to(
            serde_json::json!({ "status": prior_status }),
            serde_json::json!({ "status": "invited" }),
        ),
        serde_json::json!({ "expires_at": expires_at }),
    )
    .await;

    tracing::info!(
        admin_user_id = %admin.user_id,
        recovered_user_id = %user_id,
        "account recovery initiated"
    );

    (
        StatusCode::CREATED,
        Json(CreateInviteResponse {
            user_id,
            invite_token: raw_token,
            expires_at,
            reissued: true,
        }),
    )
        .into_response()
}

pub(super) enum RecoveryOutcome {
    Recovered {
        user_id: Uuid,
        /// The account's status immediately before this recovery cycled
        /// it through `deactivated` to `invited` -- always `active`, since
        /// that is the only status that reaches this branch (see the
        /// `status != "active"` refusal above), but carried as data rather
        /// than assumed at the call site so the audit `before_state`
        /// reflects what was actually read, not what the caller expects.
        prior_status: String,
    },
    /// `user_id` is `None` only when no account with this email exists at
    /// all -- every other refusal reason resolves to a real account
    /// first.
    Refused {
        user_id: Option<Uuid>,
        reason: &'static str,
        message: String,
    },
}

/// All of it in one transaction, same reasoning as `issue_invite`: the
/// status flip and the new invite must not be separable, or a failure
/// between them leaves the account `invited` with none of the credentials
/// it started with and no usable way back in.
pub(super) async fn recover_account_tx(
    state: &AppState,
    admin: &AuthenticatedUser,
    email: &str,
    token_hash: &[u8],
    expires_at: chrono::DateTime<chrono::Utc>,
) -> Result<RecoveryOutcome, sqlx::Error> {
    let mut tx = begin_rls_transaction(&state.db, admin.user_id, &admin.role_keys).await?;

    let existing: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, status::text FROM auth.users WHERE email = $1::citext AND deleted_at IS NULL",
    )
    .bind(email)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((user_id, status)) = existing else {
        tx.rollback().await?;
        return Ok(RecoveryOutcome::Refused {
            user_id: None,
            reason: "no_such_account",
            message: format!("No account found for {email}."),
        });
    };

    if status != "active" {
        tx.rollback().await?;

        let (reason, message) = match status.as_str() {
            "invited" => (
                "not_yet_enrolled",
                format!(
                    "{email} has not completed enrolment yet -- reissue their setup link with \
                     the regular invite endpoint instead of recovering an account."
                ),
            ),
            "deactivated" => (
                "account_deactivated",
                format!(
                    "{email} is deactivated. Reactivating an account is a separate decision \
                     from recovering a lost credential."
                ),
            ),
            other => (
                "unrecognised_status",
                format!("{email} has an unexpected status \"{other}\"."),
            ),
        };

        return Ok(RecoveryOutcome::Refused {
            user_id: Some(user_id),
            reason,
            message,
        });
    }

    // Cycle through `deactivated` so the existing revoke-all-access-paths
    // trigger (migrations/20260730*_*.sql) does the work of wiping
    // passkeys, TOTP, live sessions, and any outstanding invite for this
    // account -- writing a second copy of those DELETEs here would be
    // exactly the kind of second place those migrations' own comments
    // warn against.
    //
    // `set_user_status` returns whether it actually updated a row, and
    // this checks it: a `false` here means the account stopped being
    // recoverable between the SELECT above and now (soft-deleted by a
    // concurrent action, in practice, given both ran inside one
    // transaction), and proceeding to insert a fresh invite for a status
    // flip that never happened would be worse than refusing.
    let deactivated: bool =
        sqlx::query_scalar("SELECT auth.set_user_status($1, 'deactivated'::auth.user_status)")
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await?;

    if !deactivated {
        tx.rollback().await?;
        return Ok(RecoveryOutcome::Refused {
            user_id: Some(user_id),
            reason: "account_changed_concurrently",
            message: format!(
                "{email} could not be recovered -- its status changed while this request was \
                 in progress. Check its current state and try again."
            ),
        });
    }

    // The row is now locked by the UPDATE inside the call above and held
    // until this transaction commits or rolls back, so nothing can
    // interleave between here and the commit -- this second call cannot
    // race the way the first one could.
    sqlx::query("SELECT auth.set_user_status($1, 'invited'::auth.user_status)")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "INSERT INTO auth.user_invites (user_id, token_hash, expires_at)
         VALUES ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(token_hash)
    .bind(expires_at)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(RecoveryOutcome::Recovered {
        user_id,
        prior_status: status,
    })
}
