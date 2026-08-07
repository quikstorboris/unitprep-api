//! WebAuthn passkey registration, HTTP side (Phase 2 task 4). The
//! cryptographic work itself lives behind `auth::AuthBackend` (see
//! `auth/mod.rs`); everything here is orchestration -- deciding *who* is
//! allowed to register, persisting the ceremony state between the two
//! requests a WebAuthn ceremony inherently needs, and writing the
//! resulting credential.
//!
//! ## Why there are two endpoints
//!
//! A WebAuthn registration is always a browser round trip: `/begin`
//! returns a challenge, page JS passes it to
//! `navigator.credentials.create()`, and `/finish` verifies whatever the
//! authenticator produced. The server-side state linking the two
//! (`PasskeyRegistration`) must never be trusted from the client, so it
//! is held server-side and referenced by a short-lived opaque cookie --
//! the same shape as the real session cookie, and for the same reason.
//!
//! ## Who is allowed to register
//!
//! Two paths, decided once in `/begin`:
//!
//! 1. **Authenticated** -- a caller with a valid session registers an
//!    additional passkey for *themselves*. The target user comes from
//!    the session, never from the request body. Also requires the
//!    session to be step-up elevated (`AuthenticatedUser::is_elevated`,
//!    see `auth_totp.rs`) -- planting a durable new credential is
//!    exactly the sensitive action step-up exists to gate, and a
//!    hijacked session cookie alone must not be sufficient for it.
//! 2. **Invite** -- the unauthenticated first-passkey path (Phase 2 task
//!    6), authorized by the token from an invitation link. Every
//!    eligibility rule is enforced inside
//!    `auth.resolve_invite_registration`: the invite must be unused and
//!    unexpired, the user must still be `invited`, and they must have
//!    **zero** existing WebAuthn credentials. Those live in the SECURITY
//!    DEFINER function rather than here deliberately -- an anonymous
//!    caller therefore cannot enumerate users (see
//!    `registration_unavailable`) and cannot enrol a competing passkey
//!    over an existing one, regardless of what this handler does.
//!
//! This replaced an env-gated `AUTH_BOOTSTRAP_ENABLED` bootstrap path,
//! which is now **deleted rather than merely unset** along with its
//! `auth.resolve_bootstrap_registration` lookup. The first administrator
//! is created by `unitprep bootstrap-admin` as an `invited` user holding
//! an invite, so they walk exactly this path too -- one enrolment route
//! exercised from the very first account, rather than a special case that
//! runs once and is therefore never really tested.
//!
//! ## When the invite is consumed, and why it is not sooner
//!
//! The invite is consumed at `/finish`, **after** the credential
//! verifies, **in the same transaction** that writes the credential. Both
//! halves of that matter, and each rules out a distinct lockout:
//!
//! * Consuming at `/begin` (or in any separate step before enrolment
//!   succeeds) would mean a cancelled authenticator prompt leaves the
//!   account `active` with no credential and a spent invite. Nothing can
//!   recover that: `bootstrap-admin --reissue-invite` deliberately
//!   refuses an account that is no longer `invited`.
//! * Consuming outside the credential transaction would leave the mirror
//!   image if either statement failed -- an `invited` user who already
//!   holds a passkey, which `--reissue-invite` also refuses (it declines
//!   any account with a credential enrolled).
//!
//! One transaction makes it all-or-nothing, so the only two reachable
//! outcomes are "enrolled and active" or "untouched and retryable".
//!
//! ## Rejections are recorded even though they are not explained
//!
//! Every refusal here returns the same opaque 403 (see
//! `registration_unavailable`) *and* writes a `registration_failed` audit row
//! naming the actual reason. Those are not in tension: the response is
//! deliberately indistinguishable so this endpoint cannot be used to
//! enumerate users, while the audit row exists so an operator can see
//! probing that the attacker believes is silent. Recording it server-side
//! leaks nothing. Before this existed, a refused registration was written
//! nowhere at all while a failed *login* wrote a row -- so the identical
//! attack was visible against one endpoint and invisible against the
//! other, which was an oversight rather than a policy.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Only the Ext trait needs importing: `save`/`delete`/`get_handle` are
// called on `Arc<dyn SessionStore<_>>` directly, so that trait is
// already in scope via the type itself.
use unitprep_core::session_store::SessionStoreExt;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{
    action_requires_step_up, audit_log, begin_owner_rls_transaction, begin_rls_transaction,
    clear_ceremony_cookie, generate_token, hash_token, issue_ceremony_cookie, issue_session_cookie,
    read_ceremony_cookie, step_up_required, try_authenticated_user, RegisteredCredential,
    RegistrationCeremony, ADD_PASSKEY, REGISTRATION_CEREMONY_COOKIE,
};

/// How long a started-but-unfinished ceremony stays valid. Deliberately
/// the same 5 minutes as the ceremony store's own timeout in `main.rs` --
/// the cookie expiring and the server-side state expiring must not
/// disagree, or one of the two silently decides the real TTL.
const CEREMONY_TTL_MINUTES: i64 = 5;

/// Lifetime of a session issued by a successful invite registration.
/// Overridable per deployment, same convention as `SESSION_TIMEOUT_SECS`
/// / `HOST` / `PORT` in `main.rs`. A non-positive or unparseable value
/// falls back to the default rather than producing an
/// already-expired session.
fn session_lifetime_hours() -> i64 {
    std::env::var("SESSION_LIFETIME_HOURS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|hours| *hours > 0)
        .unwrap_or(12)
}

#[derive(Debug, Deserialize)]
pub struct RegisterBeginRequest {
    /// Raw token from the invitation link. Only consulted for an
    /// unauthenticated caller: an authenticated caller's target comes from
    /// their session and any token they send is ignored outright --
    /// honouring it would let a signed-in user start a ceremony that
    /// writes a credential onto someone else's account.
    ///
    /// There is deliberately no `email` field. The bootstrap path this
    /// replaced took one, which made the endpoint answerable by anyone who
    /// could guess an address; a token is unguessable, so possession of it
    /// *is* the authorization.
    #[serde(default)]
    pub invite_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterBeginResponse {
    /// Passed straight to `navigator.credentials.create()` by the
    /// frontend. Opaque here -- produced by the backend, not shaped by
    /// this handler.
    pub challenge: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct RegisterFinishRequest {
    /// Exactly what `navigator.credentials.create()` resolved to,
    /// relayed unmodified. Verified by the backend against the stored
    /// ceremony state; never trusted here.
    pub credential: serde_json::Value,

    /// Optional human label for the new credential ("MacBook Touch ID").
    #[serde(default)]
    pub nickname: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RegisterFinishResponse {
    pub success: bool,

    /// True when this registration also signed the caller in (invite path
    /// only -- an already-authenticated caller keeps the session they
    /// arrived with, so no new cookie is set).
    pub session_issued: bool,
}

/// Deliberately identical for "no such invite", "invite expired",
/// "invite already used", "user no longer invited", and "user already has
/// a passkey". Distinguishing them would turn this unauthenticated
/// endpoint into an oracle -- and the cases that arguably aren't secrets
/// aren't worth carving out, since carving them out is precisely what
/// reveals the others by elimination.
fn registration_unavailable() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ApiErrorBody {
            error: "registration_not_available",
            message: "Passkey registration is not available for this account.".to_string(),
        }),
    )
        .into_response()
}

/// The rejection above, plus the audit row that makes it visible to an
/// operator.
///
/// Every rejection path goes through here rather than calling
/// `registration_unavailable` directly, so "refused but recorded nowhere"
/// cannot be reintroduced by adding another reason later and forgetting
/// the audit call. `reason` lands in the audit table and never in the
/// response.
///
/// **Nothing derived from the invite token is recorded** -- not the raw
/// value, not its hash. A token is a bearer credential; an audit trail
/// containing live tokens is a credential store with a different name on
/// it. The reason alone is what an operator needs, and the deliberate
/// consequence is that a refused attempt with an unrecognised token
/// identifies no user, because there genuinely is none to identify.
///
/// `actor_user_id` is passed through rather than always `None`: once a
/// ceremony has resolved to a user, later failures for that ceremony can
/// name them honestly, and an audit row that can be joined to a user is
/// worth considerably more than one that cannot.
async fn reject_registration(
    state: &AppState,
    reason: &'static str,
    actor_user_id: Option<Uuid>,
    user_agent: Option<&str>,
) -> Response {
    // `warn`, matching a failed login ceremony: an ordinary client-side
    // outcome, not a server fault.
    tracing::warn!(reason, "passkey registration refused");

    audit_log::record(
        &state.db,
        audit_log::event::REGISTRATION_FAILED,
        // Whatever the caller resolved to, which is `None` on the paths that
        // refused before any user was identified. Never a target: nobody had
        // anything done *to* them here.
        audit_log::Subjects {
            actor: actor_user_id,
            target: None,
        },
        user_agent,
        // No ConnectInfo here -- every call site of this helper is on the
        // /begin leg, which does not take it (see login_begin for the same
        // shape of decision on the login side).
        None,
        audit_log::Change::none(),
        serde_json::json!({ "reason": reason }),
    )
    .await;

    registration_unavailable()
}

fn ceremony_not_found() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "ceremony_not_found",
            message: "This registration attempt has expired or was never started. Start again."
                .to_string(),
        }),
    )
        .into_response()
}

fn ceremony_failed() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "registration_failed",
            message: "The passkey could not be verified. Start again.".to_string(),
        }),
    )
        .into_response()
}

/// Who a ceremony is being started for, plus the names WebAuthn shows in
/// the authenticator's own UI.
struct RegistrationTarget {
    user_id: Uuid,
    username: String,
    display_name: String,

    /// Raw credential ids the authenticator should refuse to duplicate.
    /// Always empty on the invite path -- `resolve_invite_registration`
    /// only ever matches users with none, so there is nothing to exclude
    /// by construction.
    exclude: Vec<Vec<u8>>,

    /// `Some` on the invite path, carrying the hash to be consumed at
    /// `finish`. `None` for an authenticated caller adding a passkey.
    invite_token_hash: Option<Vec<u8>>,
}

pub async fn register_begin(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(request): Json<RegisterBeginRequest>,
) -> Response {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());

    // Resolved ONCE. Asking twice (a second call to decide
    // which path this is) would both waste a round trip and open a window
    // where the two answers disagree -- the path a ceremony is
    // authorized under must be a single decision, not two independent
    // lookups that happen to usually agree.
    let authenticated = try_authenticated_user(&jar, &state).await;

    let target = match authenticated {
        Some(user) => {
            // Adding a passkey to an account that already has one is
            // exactly the kind of sensitive, high-blast-radius action
            // step-up exists for -- a hijacked session cookie alone must
            // not be enough to plant a durable new credential. The
            // invite path below needs no equivalent check: possession of
            // the (unguessable) token *is* its authorization, and there
            // is no existing session to step up.
            //
            // Gated by admin-configurable policy
            // (auth.auth_configuration.step_up_actions) rather than a
            // hardcoded `true`, so which self-service actions require
            // step-up can be tuned without a code change -- see
            // auth::step_up_policy.
            let requires_step_up =
                match action_requires_step_up(&state.db, user.user_id, ADD_PASSKEY).await {
                    Ok(requires_step_up) => requires_step_up,
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            user_id = %user.user_id,
                            "failed to read step-up policy for add_passkey"
                        );
                        return internal_error("Could not start passkey registration");
                    }
                };

            if requires_step_up && !user.is_elevated() {
                return step_up_required();
            }

            match authenticated_target(&state, user.user_id, &user.role_keys).await {
                Ok(Some(target)) => target,
                // A session that resolved but whose user row is missing or
                // invisible is a real inconsistency, not a bad request --
                // `resolve_session` already vouched for that user being
                // active and non-deleted.
                Ok(None) => return internal_error("Could not load the signed-in user"),
                Err(err) => {
                    tracing::error!(error = %err, "failed to load authenticated registration target");
                    return internal_error("Could not load the signed-in user");
                }
            }
        }

        None => {
            let Some(raw_token) = request
                .invite_token
                .as_deref()
                .map(str::trim)
                .filter(|token| !token.is_empty())
            else {
                return reject_registration(&state, "missing_invite_token", None, user_agent).await;
            };

            // Hashed immediately, and only the hash travels any further --
            // into the lookup, into the ceremony, and eventually into
            // `consume_invite`. The raw token exists solely for the length
            // of this scope. Same discipline as the session cookie: the
            // database stores hashes, so nothing else should hold the
            // plaintext either.
            let token_hash = hash_token(raw_token);

            match invite_target(&state, &token_hash).await {
                Ok(Some(target)) => target,
                Ok(None) => {
                    return reject_registration(&state, "invite_not_usable", None, user_agent).await
                }
                Err(err) => {
                    tracing::error!(error = %err, "invite registration lookup failed");
                    return internal_error("Could not start passkey registration");
                }
            }
        }
    };

    let is_invite = target.invite_token_hash.is_some();

    let challenge = match state.auth_backend.start_registration(
        target.user_id,
        &target.username,
        &target.display_name,
        &target.exclude,
    ) {
        Ok(challenge) => challenge,
        Err(err) => {
            tracing::error!(error = %err, "failed to start passkey registration ceremony");
            return internal_error("Could not start passkey registration");
        }
    };

    let ceremony_id = Uuid::new_v4().to_string();

    let ceremony = RegistrationCeremony::new(
        ceremony_id.clone(),
        target.user_id,
        challenge.state,
        target.invite_token_hash,
    );

    // Read before the store takes ownership. Logged in place of
    // `ceremony_id`, which is the cookie's own value -- see
    // `RegistrationCeremony::correlation_id`.
    let correlation_id = ceremony.correlation_id;

    state.registration_ceremonies.save(ceremony);

    let jar = issue_ceremony_cookie(
        jar,
        REGISTRATION_CEREMONY_COOKIE,
        ceremony_id,
        time::Duration::minutes(CEREMONY_TTL_MINUTES),
    );

    tracing::info!(
        user_id = %target.user_id,
        correlation_id = %correlation_id,
        is_invite,
        "passkey registration ceremony started"
    );

    (
        jar,
        Json(RegisterBeginResponse {
            challenge: challenge.challenge,
        }),
    )
        .into_response()
}

/// The signed-in caller's own row plus their existing credential ids.
/// Runs inside an RLS transaction under their own identity, so the
/// database enforces "own row only" independently of this query's own
/// WHERE clause -- the WHERE is not the security boundary here.
async fn authenticated_target(
    state: &AppState,
    user_id: Uuid,
    role_keys: &[String],
) -> Result<Option<RegistrationTarget>, sqlx::Error> {
    let mut tx = begin_rls_transaction(&state.db, user_id, role_keys).await?;

    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT email::text, first_name, last_name FROM auth.users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;

    let Some((email, first_name, last_name)) = row else {
        tx.rollback().await?;
        return Ok(None);
    };

    let exclude: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT credential_id FROM auth.webauthn_credentials WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Some(RegistrationTarget {
        user_id,
        username: email,
        display_name: format!("{first_name} {last_name}"),
        exclude,
        // No invite involved: this caller already has a session, so there
        // is nothing to consume and nothing to activate.
        invite_token_hash: None,
    }))
}

/// The unauthenticated first-passkey path. Every authorization check
/// lives inside `auth.resolve_invite_registration` (invite unused and
/// unexpired, user still `invited`, zero existing credentials) -- a `None`
/// here means "not usable", with the specific reason deliberately not
/// reported back to the caller.
///
/// The email comes back from the lookup rather than from the request: the
/// caller supplies only a token, and the address WebAuthn shows in the
/// authenticator's own prompt must be the invited account's real one, not
/// anything the client could influence.
async fn invite_target(
    state: &AppState,
    token_hash: &[u8],
) -> Result<Option<RegistrationTarget>, sqlx::Error> {
    let row: Option<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT user_id, email, first_name, last_name
           FROM auth.resolve_invite_registration($1)",
    )
    .bind(token_hash)
    .fetch_optional(&state.db)
    .await?;

    Ok(row.map(
        |(user_id, email, first_name, last_name)| RegistrationTarget {
            user_id,
            username: email,
            display_name: format!("{first_name} {last_name}"),
            exclude: Vec::new(),
            invite_token_hash: Some(token_hash.to_vec()),
        },
    ))
}

pub async fn register_finish(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(request): Json<RegisterFinishRequest>,
) -> Response {
    // Read up front rather than at the point of the success audit row:
    // the failure path below needs it too, and a value extracted twice is
    // a value that eventually gets extracted differently in one of the
    // two places.
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());

    let Some(ceremony_id) = read_ceremony_cookie(&jar, REGISTRATION_CEREMONY_COOKIE) else {
        return ceremony_not_found();
    };

    // Read what's needed and release the lock before any `.await` --
    // holding a session lock across an await point is explicitly
    // forbidden by `SessionStore`'s documented locking invariants, and
    // everything below this point is async.
    let Some((user_id, correlation_id, webauthn_state, invite_token_hash)) = state
        .registration_ceremonies
        .with_session(&ceremony_id, |ceremony| {
            (
                ceremony.user_id,
                ceremony.correlation_id,
                ceremony.webauthn_state.clone(),
                ceremony.invite_token_hash.clone(),
            )
        })
    else {
        return (
            clear_ceremony_cookie(jar, REGISTRATION_CEREMONY_COOKIE),
            ceremony_not_found(),
        )
            .into_response();
    };

    // Single-use, and consumed BEFORE verification rather than after: a
    // failed or replayed attempt must not get a second try against the
    // same challenge. A legitimate retry needs a fresh `/begin`.
    state.registration_ceremonies.delete(&ceremony_id);

    let jar = clear_ceremony_cookie(jar, REGISTRATION_CEREMONY_COOKIE);

    let stored = match state
        .auth_backend
        .finish_registration(request.credential, &webauthn_state)
    {
        Ok(stored) => stored,
        Err(err) => {
            // `warn`, not `error`: a failed ceremony is an ordinary
            // client-side outcome (user cancelled, wrong device, stale
            // challenge), not a server fault.
            tracing::warn!(
                user_id = %user_id,
                correlation_id = %correlation_id,
                error = %err,
                "passkey registration ceremony failed verification"
            );

            // The registration-side counterpart of login's
            // `assertion_rejected` row. Without it, a ceremony that
            // started and then failed verification appeared in the ops
            // log and nowhere permanent -- the same gap as the refused
            // `/begin`, one step further along.
            audit_log::record(
                &state.db,
                audit_log::event::REGISTRATION_FAILED,
                audit_log::Subjects::by(user_id),
                user_agent,
                Some(sqlx::types::ipnetwork::IpNetwork::from(addr.ip())),
                audit_log::Change::none(),
                serde_json::json!({
                    "reason": "credential_rejected",
                    "correlation_id": correlation_id,
                }),
            )
            .await;

            return (jar, ceremony_failed()).into_response();
        }
    };

    let is_invite = invite_token_hash.is_some();

    match enrol_credential(
        &state,
        user_id,
        &stored,
        request.nickname.as_deref(),
        invite_token_hash.as_deref(),
    )
    .await
    {
        Ok(true) => {}

        // The credential verified, but the invite was no longer consumable
        // by the time the transaction ran -- it expired mid-ceremony, or a
        // concurrent attempt used it first. Nothing was written: the
        // transaction rolled back, so the user is still `invited` with no
        // credential and can retry with a fresh invite.
        Ok(false) => {
            tracing::warn!(
                user_id = %user_id,
                correlation_id = %correlation_id,
                "invite was no longer consumable when the credential was ready"
            );

            audit_log::record(
                &state.db,
                audit_log::event::REGISTRATION_FAILED,
                audit_log::Subjects::by(user_id),
                user_agent,
                Some(sqlx::types::ipnetwork::IpNetwork::from(addr.ip())),
                audit_log::Change::none(),
                serde_json::json!({
                    "reason": "invite_consumed_elsewhere",
                    "correlation_id": correlation_id,
                }),
            )
            .await;

            return (jar, registration_unavailable()).into_response();
        }

        Err(err) => {
            tracing::error!(
                error = %err,
                user_id = %user_id,
                correlation_id = %correlation_id,
                "failed to persist passkey credential"
            );
            return (jar, internal_error("Could not save the passkey")).into_response();
        }
    }

    audit_log::record(
        &state.db,
        audit_log::event::PASSKEY_REGISTERED,
        audit_log::Subjects::by(user_id),
        user_agent,
        Some(sqlx::types::ipnetwork::IpNetwork::from(addr.ip())),
        audit_log::Change::none(),
        // `device_bound` is captured here rather than left to be read off
        // the credential row later. The flag exists purely for admin
        // visibility, and what an admin wants to know is what the
        // authenticator claimed *at enrolment* -- a value re-read from the
        // row months later cannot distinguish "enrolled as synced" from
        // "row edited since".
        serde_json::json!({
            "invite": is_invite,
            "device_bound": stored.device_bound,
            "correlation_id": correlation_id,
        }),
    )
    .await;

    if !is_invite {
        tracing::info!(
            user_id = %user_id,
            correlation_id = %correlation_id,
            "additional passkey registered"
        );

        return (
            jar,
            Json(RegisterFinishResponse {
                success: true,
                session_issued: false,
            }),
        )
            .into_response();
    }

    // Invite path only: it ends with the user signed in, which is what
    // makes accepting an invitation a single continuous act rather than
    // "enrol, then go and sign in separately". The invite is already
    // consumed and the account already `active` by this point, which is
    // exactly why `create_session` below can succeed -- it requires an
    // active user.
    let (raw_token, token_hash) = generate_token();

    let lifetime_hours = session_lifetime_hours();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(lifetime_hours);

    // requires_step_up is always false here -- this is a brand-new
    // enrolment, not a login with session history to compare against, so
    // the Phase II anomaly signal (see auth_login.rs's assess_login_risk)
    // has nothing to evaluate. ip_address is captured for real now: the
    // into_make_service_with_connect_info wiring this comment used to wait
    // on already exists in main.rs (added for the auth rate limiter), and
    // direct exposure (no reverse proxy) is this deployment's actual
    // topology today, so the raw peer address is trustworthy without a
    // forwarded-header policy. Revisit if a reverse proxy/CDN is ever put
    // in front of this service.
    let created: Result<Uuid, sqlx::Error> =
        sqlx::query_scalar("SELECT auth.create_session($1, $2, $3, $4, $5, false)")
            .bind(user_id)
            .bind(&token_hash)
            .bind(expires_at)
            .bind(sqlx::types::ipnetwork::IpNetwork::from(addr.ip()))
            .bind(user_agent)
            .fetch_one(&state.db)
            .await;

    match created {
        Ok(session_id) => {
            tracing::info!(
                user_id = %user_id,
                session_id = %session_id,
                correlation_id = %correlation_id,
                "invite accepted, passkey registered and session issued"
            );

            let jar = issue_session_cookie(jar, raw_token, time::Duration::hours(lifetime_hours));

            (
                jar,
                Json(RegisterFinishResponse {
                    success: true,
                    session_issued: true,
                }),
            )
                .into_response()
        }

        Err(err) => {
            // The credential IS saved at this point. Report the partial
            // outcome honestly rather than a flat failure that would
            // invite the user to re-register -- which would now hit the
            // "already has a credential" guard and look permanently
            // broken. Signing in via the normal login flow (task 5)
            // works; nothing needs redoing.
            tracing::error!(
                error = %err,
                user_id = %user_id,
                correlation_id = %correlation_id,
                "passkey saved but session creation failed"
            );

            (
                jar,
                internal_error("Passkey saved, but sign-in failed — try signing in"),
            )
                .into_response()
        }
    }
}

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
async fn enrol_credential(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::empty_state;

    /// A stand-in peer address for tests -- `register_finish` now takes
    /// `ConnectInfo<SocketAddr>` (only populated for real by
    /// `into_make_service_with_connect_info` outside of tests). The actual
    /// value never matters here: every test below fails before reaching
    /// create_session, against the unreachable test pool.
    fn test_addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    /// An unauthenticated caller with no invite token has nothing that
    /// could authorize a registration, and must be refused without the
    /// database being consulted at all.
    ///
    /// Replaces two earlier tests that asserted the `AUTH_BOOTSTRAP_ENABLED`
    /// env gate held shut. That gate is gone: there is no env var to leave
    /// unset, because there is no longer an unauthenticated path that a
    /// deployment could accidentally open. Possession of an unguessable
    /// token is now the only authorization, which is a stronger property
    /// than a correctly-configured flag.
    #[tokio::test]
    async fn begin_refuses_an_unauthenticated_caller_without_an_invite_token() {
        // `empty_state`'s pool is lazy and points at nothing reachable, so
        // a 403 here also proves no lookup was attempted -- a query would
        // surface as a 500.
        let response = register_begin(
            State(empty_state()),
            CookieJar::new(),
            HeaderMap::new(),
            Json(RegisterBeginRequest { invite_token: None }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// The env var that used to gate this path must not still be consulted
    /// anywhere. Setting it to the value that previously opened the gate
    /// must now change nothing whatsoever.
    ///
    /// Written because "delete the gate" is easy to do incompletely: a
    /// leftover read in one branch would restore an unauthenticated path
    /// that no test named, and the vault's standing instruction was that
    /// this variable be *deleted*, not merely unset.
    #[tokio::test]
    async fn the_old_bootstrap_env_var_no_longer_opens_anything() {
        std::env::set_var("AUTH_BOOTSTRAP_ENABLED", "true");

        let response = register_begin(
            State(empty_state()),
            CookieJar::new(),
            HeaderMap::new(),
            Json(RegisterBeginRequest { invite_token: None }),
        )
        .await;

        std::env::remove_var("AUTH_BOOTSTRAP_ENABLED");

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "AUTH_BOOTSTRAP_ENABLED must be inert -- if this fails, a read of it survived the removal"
        );
    }

    /// Every rejection reason must produce a byte-identical response.
    /// This is the anti-enumeration property, and it is exactly what the
    /// new audit rows could have broken -- recording a distinct `reason`
    /// server-side is only safe while none of it reaches the caller. The
    /// body is compared, not just the status: a `reason` leaking into the
    /// error payload would keep the status at 403 and still hand an
    /// attacker the oracle.
    #[tokio::test]
    async fn every_rejection_reason_returns_an_identical_response() {
        async fn body_of(response: Response) -> (StatusCode, Vec<u8>) {
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("response body should be readable");
            (status, bytes.to_vec())
        }

        // No token at all -- refused before any lookup.
        let no_token = body_of(
            register_begin(
                State(empty_state()),
                CookieJar::new(),
                HeaderMap::new(),
                Json(RegisterBeginRequest { invite_token: None }),
            )
            .await,
        )
        .await;

        // Whitespace-only, which trims to nothing and takes the same
        // branch.
        let blank_token = body_of(
            register_begin(
                State(empty_state()),
                CookieJar::new(),
                HeaderMap::new(),
                Json(RegisterBeginRequest {
                    invite_token: Some("   ".to_string()),
                }),
            )
            .await,
        )
        .await;

        assert_eq!(no_token.0, StatusCode::FORBIDDEN);
        assert_eq!(
            no_token, blank_token,
            "a blank token must not be distinguishable from a missing one"
        );
    }

    /// The invite token must never reach the response, in any form. A
    /// token echoed back into an error body would be a bearer credential
    /// in a place that gets logged by proxies, screenshotted, and pasted
    /// into bug reports.
    #[tokio::test]
    async fn a_refusal_never_echoes_the_invite_token() {
        let secret = "not-a-real-token-abc123";

        let response = register_begin(
            State(empty_state()),
            CookieJar::new(),
            HeaderMap::new(),
            Json(RegisterBeginRequest {
                invite_token: Some(secret.to_string()),
            }),
        )
        .await;

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        let body = String::from_utf8_lossy(&bytes);

        assert!(
            !body.contains(secret),
            "the refusal body must not contain the submitted token: {body}"
        );
    }

    /// No cookie means there is no ceremony to finish -- and critically,
    /// that must not surface as a verification failure or a 500.
    #[tokio::test]
    async fn finish_without_a_ceremony_cookie_is_a_bad_request() {
        let response = register_finish(
            State(empty_state()),
            test_addr(),
            CookieJar::new(),
            HeaderMap::new(),
            Json(RegisterFinishRequest {
                credential: serde_json::json!({}),
                nickname: None,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// A cookie naming a ceremony the store has never heard of (expired,
    /// process restarted, or simply fabricated) is the same "start
    /// again" case as no cookie at all.
    #[tokio::test]
    async fn finish_with_an_unknown_ceremony_id_is_a_bad_request() {
        let jar = issue_ceremony_cookie(
            CookieJar::new(),
            REGISTRATION_CEREMONY_COOKIE,
            "not-a-real-ceremony".to_string(),
            time::Duration::minutes(5),
        );

        let response = register_finish(
            State(empty_state()),
            test_addr(),
            jar,
            HeaderMap::new(),
            Json(RegisterFinishRequest {
                credential: serde_json::json!({}),
                nickname: None,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// Regression guard for the single-use property: a ceremony must be
    /// gone from the store after one `/finish` attempt, INCLUDING a
    /// failing one, so the same challenge can never be retried. Written
    /// against a failing attempt specifically because that is the case a
    /// "delete on success" implementation would get wrong while still
    /// looking correct in the happy path.
    #[tokio::test]
    async fn a_failed_finish_still_consumes_the_ceremony() {
        let state = empty_state();
        let user_id = Uuid::new_v4();

        state
            .registration_ceremonies
            .save(RegistrationCeremony::new(
                "ceremony-1".to_string(),
                user_id,
                b"not-valid-webauthn-state".to_vec(),
                Some(b"invite-token-hash".to_vec()),
            ));

        let jar = issue_ceremony_cookie(
            CookieJar::new(),
            REGISTRATION_CEREMONY_COOKIE,
            "ceremony-1".to_string(),
            time::Duration::minutes(5),
        );

        let response = register_finish(
            State(state.clone()),
            test_addr(),
            jar,
            HeaderMap::new(),
            Json(RegisterFinishRequest {
                credential: serde_json::json!({ "garbage": true }),
                nickname: None,
            }),
        )
        .await;

        // Verification fails (the stored state is nonsense), but the
        // ceremony must be consumed regardless.
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        assert!(
            state
                .registration_ceremonies
                .get_handle("ceremony-1")
                .is_none(),
            "a consumed ceremony must not remain in the store"
        );
    }
}
