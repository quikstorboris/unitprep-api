//! Creating (and re-issuing) an invite for a user: request shapes, the endpoint, and the transaction that issues it.

use crate::api::rls::try_response;
use crate::api::{bad_request, conflict, internal_error, AppState};
use crate::auth::{
    audit_log, begin_rls_transaction, generate_token, resolve_role_id, AuthenticatedUser,
};
use crate::bootstrap::{invite_hours, VALID_COMPANIES};
use axum::extract::{ConnectInfo, Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use uuid::Uuid;

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

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
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
    try_response!(
        admin
            .require_permission(
                &state.db,
                "users.manage",
                "create_invite",
                user_agent,
                ip_address,
            )
            .await
    );

    // Every other path that sets a user's role (grant_role/revoke_role in
    // auth_user_role.rs) requires users.manage_roles -- this one assigns
    // a brand-new account's first role and must be held to the same bar.
    // Without this, a narrower custom role holding users.manage but not
    // users.manage_roles (a "can invite people" role, deliberately not
    // "can grant admin") could still invite someone straight in as
    // admin, fully bypassing the reason that second permission exists.
    try_response!(
        admin
            .require_permission(
                &state.db,
                "users.manage_roles",
                "create_invite",
                user_agent,
                ip_address,
            )
            .await
    );

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
            // A deliberately *explicit* conflict, unlike the opaque
            // refusals on the unauthenticated endpoints -- the caller
            // here is an authenticated administrator who can already
            // list users, so withholding the reason protects nothing
            // and costs them the ability to act on it. Anti-enumeration
            // reasoning applies to anonymous callers; applying it to an
            // admin tool just makes the tool worse.
            return conflict("invite_not_applicable", message);
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
pub(super) struct IssueInvite<'a> {
    pub(super) email: &'a str,
    pub(super) first_name: &'a str,
    pub(super) last_name: &'a str,
    pub(super) company: &'a str,
    pub(super) job_title: Option<&'a str>,
    pub(super) role: &'a str,
    pub(super) token_hash: &'a [u8],
    pub(super) expires_at: chrono::DateTime<chrono::Utc>,
}

pub(super) enum Outcome {
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
pub(super) enum IssueInviteError {
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
pub(super) async fn issue_invite(
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
