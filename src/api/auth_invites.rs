//! Invitation creation, admin-only (Phase 2 task 7). The other half of the
//! invitation flow: task 6 made a token redeemable, this makes one
//! issuable by an administrator rather than only by the
//! `unitprep bootstrap-admin` CLI.
//!
//! ## Why this needs no SECURITY DEFINER function
//!
//! Both writes are permitted to `app_service` directly, under RLS policies
//! that check the identity GUCs:
//!
//! - `users_insert_admin_only` — `WITH CHECK (auth.current_user_has_role('admin'))`
//! - `user_invites_admin_only` — `FOR ALL` under the same condition
//!
//! So running inside `begin_rls_transaction(.., &admin.role_keys)` means the
//! **database** enforces admin-ness independently of this handler's own
//! check. Both exist on purpose: the handler's check produces a clean 403,
//! and the policy is what holds if a future refactor forgets it.
//!
//! `bootstrap-admin` needs the owner connection instead, for a reason that
//! does not apply here: at bootstrap time no administrator exists yet, so
//! there is no identity to put in the GUC and nothing for the policy to
//! approve. Creating the *first* user is genuinely a different problem from
//! creating the second.
//!
//! `user_invites.created_by` is left to its column default, which reads
//! `app.current_user_id` — so inside this transaction it records the issuing
//! admin by itself. That default was written for exactly this call site;
//! binding it explicitly would be duplicating the schema's own answer.
//!
//! ## Role validity now costs a database round trip
//!
//! A role key can no longer be checked against a closed Rust enum before
//! opening a transaction -- roles are real data (`auth.roles`), open-ended
//! rather than a fixed pair, so the only source of truth for "is this a
//! real role" is the table itself. `issue_invite` resolves it via
//! `resolve_role_id` right after opening the RLS transaction, and rolls
//! back cleanly on an unknown key rather than attempting the write.
//!
//! ## No email is sent
//!
//! There is no ESP integration yet, so the response returns the raw token
//! **once** and delivering it is the admin's problem. That is a deliberate
//! staging point, not an oversight: notify-on-enrolment was chosen over a
//! blocking approval step, and both wait on an ESP existing.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{
    audit_log, begin_rls_transaction, generate_token, resolve_role_id, AuthenticatedUser,
};
use crate::bootstrap::{invite_hours, VALID_COMPANIES};

#[derive(Debug, Deserialize)]
pub struct CreateInviteRequest {
    pub email: String,
    pub first_name: String,
    pub last_name: String,

    /// One of `VALID_COMPANIES`, mirroring the `auth.user_company` enum.
    pub company: String,

    #[serde(default)]
    pub job_title: Option<String>,

    /// Any key currently in `auth.roles` -- resolved and validated inside
    /// `issue_invite`'s own transaction (see the module doc for why that
    /// can no longer happen before one opens). Any admin may assign any
    /// role that exists today.
    pub role: String,
}

#[derive(Debug, Serialize)]
pub struct CreateInviteResponse {
    pub user_id: Uuid,

    /// The raw invitation token, returned **once**. Only its hash is
    /// stored, so this response is the sole opportunity to capture it --
    /// same property as the bootstrap CLI's printed link, and the reason
    /// there is a reissue path at all.
    pub invite_token: String,

    pub expires_at: chrono::DateTime<chrono::Utc>,

    /// True when this replaced an outstanding invite for an account that
    /// already existed, rather than creating a new account.
    pub reissued: bool,
}

fn bad_request(error: &'static str, message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody { error, message }),
    )
        .into_response()
}

/// A deliberately *explicit* conflict, unlike the opaque refusals on the
/// unauthenticated endpoints.
///
/// The caller here is an authenticated administrator who can already list
/// users, so withholding the reason protects nothing and costs them the
/// ability to act on it. Anti-enumeration reasoning applies to anonymous
/// callers; applying it to an admin tool just makes the tool worse.
fn conflict(message: String) -> Response {
    (
        StatusCode::CONFLICT,
        Json(ApiErrorBody {
            error: "invite_not_applicable",
            message,
        }),
    )
        .into_response()
}

pub async fn create_invite(
    State(state): State<AppState>,
    admin: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<CreateInviteRequest>,
) -> Response {
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    // Redundant with the RLS policy by design, not by accident -- see the
    // module docs.
    if let Err(response) = admin
        .require_permission(
            &state.db,
            "users.manage",
            "create_invite",
            user_agent,
            ip_address,
        )
        .await
    {
        return response;
    }

    // Every other path that sets a user's role (grant_role/revoke_role in
    // auth_user_role.rs) requires users.manage_roles -- this one assigns
    // a brand-new account's first role and must be held to the same bar.
    // Without this, a narrower custom role holding users.manage but not
    // users.manage_roles (a "can invite people" role, deliberately not
    // "can grant admin") could still invite someone straight in as
    // admin, fully bypassing the reason that second permission exists.
    if let Err(response) = admin
        .require_permission(
            &state.db,
            "users.manage_roles",
            "create_invite",
            user_agent,
            ip_address,
        )
        .await
    {
        return response;
    }

    let email = request.email.trim().to_ascii_lowercase();
    let first_name = request.first_name.trim();
    let last_name = request.last_name.trim();
    let company = request.company.trim().to_ascii_lowercase();
    let job_title = request
        .job_title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let role_key = request.role.trim().to_ascii_lowercase();

    // Validated before a transaction is opened, so a typo costs a round trip
    // rather than a rolled-back write -- and so `company` fails with the
    // valid options named instead of a Postgres enum cast error. Same
    // reasoning as the bootstrap CLI, whose list this reuses so the two
    // cannot disagree about what a company is. `role` cannot join this
    // group of checks any more -- see the module doc.
    if email.is_empty() || !email.contains('@') || email.split_whitespace().count() > 1 {
        return bad_request("invalid_email", "A valid email address is required.".into());
    }
    if first_name.is_empty() || last_name.is_empty() {
        return bad_request(
            "invalid_name",
            "Both first_name and last_name are required.".into(),
        );
    }
    if !VALID_COMPANIES.contains(&company.as_str()) {
        return bad_request(
            "invalid_company",
            format!("company must be one of: {}", VALID_COMPANIES.join(", ")),
        );
    }
    if role_key.is_empty() {
        return bad_request("invalid_role", "role is required.".to_string());
    }

    let (raw_token, token_hash) = generate_token();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(invite_hours());

    let outcome = issue_invite(
        &state,
        &admin,
        IssueInvite {
            email: &email,
            first_name,
            last_name,
            company: &company,
            job_title,
            role: &role_key,
            token_hash: &token_hash,
            expires_at,
        },
    )
    .await;

    let (user_id, reissued) = match outcome {
        Ok(Outcome::Issued { user_id, reissued }) => (user_id, reissued),

        Ok(Outcome::Refused {
            user_id,
            reason,
            message,
        }) => {
            // Unlike the unauthenticated registration/login paths, there is
            // no anti-enumeration reason to withhold this from the caller
            // (an authenticated admin who can already see the user list) --
            // but the attempt itself is still worth a permanent row, for
            // the same reason a successful invite gets one: it is an
            // administrative act performed on a specific account, and
            // "attempted but refused" is a different fact from "never
            // attempted at all".
            audit_log::record(
                &state.db,
                audit_log::event::INVITE_REFUSED,
                audit_log::Subjects::by(admin.user_id).about(user_id),
                user_agent,
                ip_address,
                audit_log::Change::none(),
                serde_json::json!({ "reason": reason }),
            )
            .await;

            tracing::info!(
                admin_user_id = %admin.user_id,
                target_user_id = %user_id,
                reason,
                "invite refused"
            );
            return conflict(message);
        }

        Err(IssueInviteError::InvalidRole(role_key)) => {
            return bad_request("invalid_role", format!("No such role: {role_key}"));
        }

        Err(IssueInviteError::Database(err)) => {
            tracing::error!(
                error = %err,
                admin_user_id = %admin.user_id,
                "failed to issue an invitation"
            );
            return internal_error("Could not create the invitation");
        }
    };

    audit_log::record(
        &state.db,
        audit_log::event::INVITE_CREATED,
        // The first event in this codebase where actor and target are
        // genuinely different people: an administrator acted, someone else
        // was acted upon.
        audit_log::Subjects::by(admin.user_id).about(user_id),
        user_agent,
        ip_address,
        audit_log::Change::none(),
        // No token and no hash. The invite is a bearer credential and the
        // audit trail is not a place to keep one. `expires_at` is what an
        // operator actually needs to reason about later.
        serde_json::json!({
            "reissued": reissued,
            "role": role_key,
            "expires_at": expires_at,
        }),
    )
    .await;

    tracing::info!(
        admin_user_id = %admin.user_id,
        invited_user_id = %user_id,
        reissued,
        "invitation issued"
    );

    (
        StatusCode::CREATED,
        Json(CreateInviteResponse {
            user_id,
            invite_token: raw_token,
            expires_at,
            reissued,
        }),
    )
        .into_response()
}

/// Everything the write needs, grouped so the helper does not take nine
/// positional arguments (four of them adjacent `&str`s, which is how a
/// first name ends up in the company column).
struct IssueInvite<'a> {
    email: &'a str,
    first_name: &'a str,
    last_name: &'a str,
    company: &'a str,
    job_title: Option<&'a str>,
    role: &'a str,
    token_hash: &'a [u8],
    expires_at: chrono::DateTime<chrono::Utc>,
}

enum Outcome {
    Issued {
        user_id: Uuid,
        reissued: bool,
    },
    /// A legitimate "no", with a message safe to show an administrator.
    /// `user_id` names the existing account the attempt was about --
    /// `Refused` only ever happens once an existing row was found -- and
    /// `reason` is the structured counterpart of `message`: the audit
    /// trail gets a stable code, the admin gets a full sentence.
    Refused {
        user_id: Uuid,
        reason: &'static str,
        message: String,
    },
}

/// `issue_invite`'s error type -- a plain `sqlx::Error` is no longer
/// enough now that "the submitted role doesn't exist" is a real outcome
/// discovered mid-transaction rather than a pre-transaction parse
/// failure. `From<sqlx::Error>` keeps every existing `?` inside
/// `issue_invite` working unchanged.
enum IssueInviteError {
    Database(sqlx::Error),
    InvalidRole(String),
}

impl From<sqlx::Error> for IssueInviteError {
    fn from(err: sqlx::Error) -> Self {
        IssueInviteError::Database(err)
    }
}

/// Creates the account if the address is new, or reissues for an account
/// that is still awaiting its first enrolment.
///
/// All of it in one transaction: retiring the previous invite and minting
/// the replacement must not be separable, or a failure between them leaves
/// an account with no usable invite at all and no way for the admin to tell
/// whether the old one still works.
async fn issue_invite(
    state: &AppState,
    admin: &AuthenticatedUser,
    input: IssueInvite<'_>,
) -> Result<Outcome, IssueInviteError> {
    let mut tx = begin_rls_transaction(&state.db, admin.user_id, &admin.role_keys).await?;

    let role_id = match resolve_role_id(&mut tx, input.role).await? {
        Some(id) => id,
        None => {
            tx.rollback().await?;
            return Err(IssueInviteError::InvalidRole(input.role.to_string()));
        }
    };

    // Soft-deleted accounts are excluded, so the address of a removed user
    // can be re-invited rather than being permanently unusable. That matters
    // because a user with audit history cannot be hard-deleted, so without
    // this the first mistake with an address would burn it forever.
    let existing: Option<(Uuid, String, i64)> = sqlx::query_as(
        "SELECT u.id, u.status::text,
                (SELECT count(*) FROM auth.webauthn_credentials c WHERE c.user_id = u.id)
           FROM auth.users u
          WHERE u.email = $1::citext AND u.deleted_at IS NULL",
    )
    .bind(input.email)
    .fetch_optional(&mut *tx)
    .await?;

    let (user_id, reissued) = match existing {
        None => {
            let user_id: Uuid = sqlx::query_scalar(
                "INSERT INTO auth.users
                     (email, first_name, last_name, job_title, company, status)
                 VALUES ($1::citext, $2, $3, $4, $5::auth.user_company,
                         'invited'::auth.user_status)
                 RETURNING id",
            )
            .bind(input.email)
            .bind(input.first_name)
            .bind(input.last_name)
            .bind(input.job_title)
            .bind(input.company)
            .fetch_one(&mut *tx)
            .await?;

            sqlx::query(
                "INSERT INTO auth.user_roles (user_id, role_id, granted_by) VALUES ($1, $2, $3)",
            )
            .bind(user_id)
            .bind(role_id)
            .bind(admin.user_id)
            .execute(&mut *tx)
            .await?;

            (user_id, false)
        }

        Some((id, status, credential_count)) => {
            // Refusals mirror `bootstrap-admin --reissue-invite` exactly.
            // Two tools that both mint invites must agree on when an invite
            // is meaningless, or "it worked from the CLI" becomes a real
            // support conversation.
            if credential_count > 0 {
                tx.rollback().await?;
                return Ok(Outcome::Refused {
                    user_id: id,
                    reason: "already_credentialed",
                    message: format!(
                        "{} already has {credential_count} passkey(s) enrolled and can sign in \
                         normally. An invitation is only for an account that has never enrolled.",
                        input.email
                    ),
                });
            }

            if status != "invited" {
                tx.rollback().await?;
                return Ok(Outcome::Refused {
                    user_id: id,
                    reason: "not_invited_status",
                    message: format!(
                        "{} has status \"{status}\", not \"invited\". An invitation is only for \
                         an account still awaiting its first enrolment.",
                        input.email
                    ),
                });
            }

            // Retiring outstanding invites is what keeps at most one live
            // link per account, and it is why the "one outstanding invite
            // per user" partial-unique constraint was never needed: the
            // invariant is maintained by every path that issues, rather
            // than enforced by the schema against paths that would
            // otherwise break it. Leaving the old token usable would mean a
            // lost link stayed valid until natural expiry, which is the
            // opposite of what someone reissuing wants.
            let retired = sqlx::query(
                "UPDATE auth.user_invites SET used_at = now()
                  WHERE user_id = $1 AND used_at IS NULL",
            )
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();

            if retired > 0 {
                tracing::info!(
                    invited_user_id = %id,
                    retired,
                    "retired outstanding invite(s) before reissuing"
                );
            }

            // Re-applies whatever role was submitted, even on a reissue --
            // an account still `invited` (the only status that reaches
            // this branch) has never signed in, so there is no session or
            // established behaviour a role change here could disrupt.
            // Without this, changing the role dropdown before clicking
            // Reissue would silently do nothing, which is worse than
            // either always honouring it or not accepting it at all. Now
            // expressed as "replace the role set" (clear, then insert the
            // one submitted) rather than "set the one role column", since
            // role is no longer a single value -- same net effect for the
            // common case of one role at invite time.
            sqlx::query("DELETE FROM auth.user_roles WHERE user_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;

            sqlx::query(
                "INSERT INTO auth.user_roles (user_id, role_id, granted_by) VALUES ($1, $2, $3)",
            )
            .bind(id)
            .bind(role_id)
            .bind(admin.user_id)
            .execute(&mut *tx)
            .await?;

            (id, true)
        }
    };

    // created_by is left to its column default, which resolves to
    // `app.current_user_id` -- set by begin_rls_transaction above, so this
    // records the issuing admin without being told to.
    sqlx::query(
        "INSERT INTO auth.user_invites (user_id, token_hash, expires_at)
         VALUES ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(input.token_hash)
    .bind(input.expires_at)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Outcome::Issued { user_id, reissued })
}

#[derive(Debug, Deserialize)]
pub struct RecoverAccountRequest {
    pub email: String,
}

/// Unlike `conflict` above, this is a genuine 404 -- there really is no
/// account behind the address, which an authenticated admin is entitled
/// to be told plainly, same reasoning `conflict` already applies to every
/// other refusal on this endpoint.
fn account_not_found(email: &str) -> Response {
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
                Some(_) => conflict(message),
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

enum RecoveryOutcome {
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
async fn recover_account_tx(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{admin_user, empty_state, onboarding_manager_user};

    /// A stand-in peer address -- both handlers now take
    /// `ConnectInfo<SocketAddr>` (only populated for real by
    /// `into_make_service_with_connect_info` outside of tests), matching
    /// the same fixture already used in auth_login.rs/auth_register.rs.
    fn test_addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    fn request(email: &str, company: &str) -> CreateInviteRequest {
        CreateInviteRequest {
            email: email.to_string(),
            first_name: "Ada".to_string(),
            last_name: "Lovelace".to_string(),
            company: company.to_string(),
            job_title: None,
            role: "admin".to_string(),
        }
    }

    /// Every validation failure must be caught before the database is
    /// touched. `empty_state`'s pool points at nothing reachable, so a query
    /// surfaces as a 500 -- meaning a 400 here also proves no connection was
    /// attempted, which is the property worth having: a typo should not cost
    /// a transaction.
    #[tokio::test]
    async fn invalid_input_is_refused_without_touching_the_database() {
        let cases = [
            (request("", "quikstor"), "empty email"),
            (request("   ", "quikstor"), "whitespace email"),
            (request("not-an-address", "quikstor"), "no @ sign"),
            (
                request("a b@example.com", "quikstor"),
                "embedded whitespace",
            ),
            (request("ada@example.com", "acme"), "unknown company"),
            (request("ada@example.com", ""), "empty company"),
        ];

        for (body, label) in cases {
            let response = create_invite(
                State(empty_state()),
                admin_user(),
                test_addr(),
                HeaderMap::new(),
                Json(body),
            )
            .await;

            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "expected a 400 for {label}"
            );
        }
    }

    /// A missing name is as invalid as a missing email -- the invited person
    /// has to be addressable in the authenticator prompt, which shows the
    /// display name.
    #[tokio::test]
    async fn blank_names_are_refused() {
        let mut body = request("ada@example.com", "quikstor");
        body.first_name = "   ".to_string();

        let response = create_invite(
            State(empty_state()),
            admin_user(),
            test_addr(),
            HeaderMap::new(),
            Json(body),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// A non-admin authenticated caller must be refused before any
    /// validation of the request body even runs -- permission-gating
    /// happens first.
    #[tokio::test]
    async fn create_invite_refuses_insufficient_permission() {
        let response = create_invite(
            State(empty_state()),
            onboarding_manager_user(),
            test_addr(),
            HeaderMap::new(),
            Json(request("ada@example.com", "quikstor")),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// Regression test: a caller holding `users.manage` but not
    /// `users.manage_roles` must still be refused. Before this gate, such
    /// a role -- e.g. a narrower "can invite people" role deliberately
    /// scoped without the ability to grant admin -- could invite a
    /// brand-new account straight in as `admin`, bypassing the entire
    /// reason `users.manage_roles` exists as a separate permission from
    /// `grant_role`/`revoke_role`'s own gate.
    #[tokio::test]
    async fn create_invite_refuses_users_manage_without_users_manage_roles() {
        let narrow_role_user = crate::auth::AuthenticatedUser {
            user_id: uuid::Uuid::new_v4(),
            role_keys: vec!["custom_inviter".to_string()],
            permission_keys: ["users.manage".to_string()].into_iter().collect(),
            token_hash: vec![0u8; 32],
            elevated_until: None,
            requires_step_up: false,
            passkey_reverified_until: None,
        };

        let response = create_invite(
            State(empty_state()),
            narrow_role_user,
            test_addr(),
            HeaderMap::new(),
            Json(request("ada@example.com", "quikstor")),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// Company matching must not depend on the caller's capitalisation, and
    /// the email is lowercased before it reaches a `citext` column so two
    /// spellings of one address cannot become two accounts. Reaching the
    /// database (a 500 against the unreachable test pool) is the *success*
    /// signal here: it proves validation passed rather than rejecting a
    /// legitimate request.
    #[tokio::test]
    async fn company_and_email_casing_are_normalised_not_rejected() {
        let response = create_invite(
            State(empty_state()),
            admin_user(),
            test_addr(),
            HeaderMap::new(),
            Json(request("Ada@Example.COM", "QuikStor")),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "validation should have passed and the call should have reached the database"
        );
    }

    /// An unknown role can no longer be caught before the database is
    /// touched -- roles are real data now (see the module doc), so the
    /// only source of truth is the `auth.roles` table itself, which this
    /// handler only reaches inside a transaction. Reaching the database
    /// (a 500 against the unreachable test pool) is the success signal
    /// here: it proves every pre-transaction check passed and the role
    /// lookup was actually attempted. The "no such role" 400 itself is
    /// exercised against the real dev database, not this fake pool, same
    /// as several other DB-dependent branches in this codebase.
    #[tokio::test]
    async fn an_unrecognised_role_still_reaches_the_database() {
        let mut body = request("ada@example.com", "quikstor");
        body.role = "superuser".to_string();

        let response = create_invite(
            State(empty_state()),
            admin_user(),
            test_addr(),
            HeaderMap::new(),
            Json(body),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// A blank role is still caught before the database, same as the other
    /// required-field checks -- there is no ambiguity to resolve against
    /// `auth.roles` for an empty string.
    #[tokio::test]
    async fn a_blank_role_is_refused_without_touching_the_database() {
        let mut body = request("ada@example.com", "quikstor");
        body.role = "   ".to_string();

        let response = create_invite(
            State(empty_state()),
            admin_user(),
            test_addr(),
            HeaderMap::new(),
            Json(body),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// Any role key reaches the same code path -- reaching the database (a
    /// 500 against the unreachable test pool) is the success signal, same
    /// convention as the casing-normalisation test above.
    #[tokio::test]
    async fn onboarding_manager_is_an_accepted_role_string() {
        let mut body = request("ada@example.com", "quikstor");
        body.role = "onboarding_manager".to_string();

        let response = create_invite(
            State(empty_state()),
            admin_user(),
            test_addr(),
            HeaderMap::new(),
            Json(body),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    fn recover_request(email: &str) -> RecoverAccountRequest {
        RecoverAccountRequest {
            email: email.to_string(),
        }
    }

    /// Same property as `invalid_input_is_refused_without_touching_the_database`
    /// above, for the one field this endpoint takes.
    #[tokio::test]
    async fn recovery_rejects_an_invalid_email_without_touching_the_database() {
        let cases = [
            (recover_request(""), "empty email"),
            (recover_request("   "), "whitespace email"),
            (recover_request("not-an-address"), "no @ sign"),
            (recover_request("a b@example.com"), "embedded whitespace"),
        ];

        for (body, label) in cases {
            let response = recover_account(
                State(empty_state()),
                admin_user(),
                test_addr(),
                HeaderMap::new(),
                Json(body),
            )
            .await;

            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "expected a 400 for {label}"
            );
        }
    }

    /// Same permission-gating property as `create_invite_refuses_insufficient_permission`,
    /// on the recovery endpoint.
    #[tokio::test]
    async fn recover_account_refuses_insufficient_permission() {
        let response = recover_account(
            State(empty_state()),
            onboarding_manager_user(),
            test_addr(),
            HeaderMap::new(),
            Json(recover_request("someone@example.com")),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// A syntactically valid email must reach the database -- the 500 here
    /// (against the unreachable test pool) is the success signal, same
    /// convention as `company_and_email_casing_are_normalised_not_rejected`.
    #[tokio::test]
    async fn recovery_with_a_valid_email_reaches_the_database() {
        let response = recover_account(
            State(empty_state()),
            admin_user(),
            test_addr(),
            HeaderMap::new(),
            Json(recover_request("someone@example.com")),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
