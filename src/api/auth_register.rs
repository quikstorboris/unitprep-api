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
//!    the session, never from the request body.
//! 2. **Bootstrap** -- the unauthenticated first-passkey path, without
//!    which nobody could ever sign in at all (nothing has issued a
//!    session yet, and invite acceptance/creation are tasks 6/7). Gated
//!    three ways: the `AUTH_BOOTSTRAP_ENABLED` env var, plus two checks
//!    enforced inside `auth.resolve_bootstrap_registration` itself --
//!    the user must be `active`, and must have **zero** existing
//!    WebAuthn credentials. Those two live in the SECURITY DEFINER
//!    function rather than here deliberately: an anonymous caller
//!    therefore cannot enumerate users (see `bootstrap_rejected`) and
//!    cannot register a competing passkey over an existing one,
//!    regardless of what this handler does.
//!
//! Once tasks 5-8 land, the bootstrap path stops being the only way in
//! and `AUTH_BOOTSTRAP_ENABLED` should be left unset.

use axum::{
    extract::{Json, State},
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
    audit_log, begin_owner_rls_transaction, begin_rls_transaction, clear_ceremony_cookie,
    generate_token, issue_ceremony_cookie, issue_session_cookie, read_ceremony_cookie,
    try_authenticated_user, RegisteredCredential, RegistrationCeremony, Role,
    REGISTRATION_CEREMONY_COOKIE,
};

/// How long a started-but-unfinished ceremony stays valid. Deliberately
/// the same 5 minutes as the ceremony store's own timeout in `main.rs` --
/// the cookie expiring and the server-side state expiring must not
/// disagree, or one of the two silently decides the real TTL.
const CEREMONY_TTL_MINUTES: i64 = 5;

/// Lifetime of a session issued by a successful bootstrap registration.
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

/// Opt-in only, and only on an exact `true` -- an unset, empty, or
/// typo'd value leaves the unauthenticated path closed. This is the
/// inverse of `SESSION_COOKIE_SECURE`'s `!= "false"` default-on
/// treatment, which is correct there (secure by default) and would be
/// exactly wrong here.
fn bootstrap_enabled() -> bool {
    std::env::var("AUTH_BOOTSTRAP_ENABLED")
        .map(|value| value == "true")
        .unwrap_or(false)
}

#[derive(Debug, Deserialize)]
pub struct RegisterBeginRequest {
    /// Only consulted on the bootstrap path. An authenticated caller's
    /// target comes from their session and any `email` they send is
    /// ignored outright -- honouring it would let a signed-in user start
    /// a ceremony that writes a credential onto someone else's account.
    #[serde(default)]
    pub email: Option<String>,
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

    /// True when this registration also signed the caller in (bootstrap
    /// path only -- an already-authenticated caller keeps the session
    /// they arrived with, so no new cookie is set).
    pub session_issued: bool,
}

/// Deliberately identical for "no such email", "user not active",
/// "already has a passkey", and "bootstrap disabled". Distinguishing them
/// would turn this unauthenticated endpoint into a user-enumeration
/// oracle -- and the two cases that arguably aren't secrets (disabled /
/// already-registered) aren't worth carving out, since carving them out
/// is precisely what reveals the other two by elimination.
fn bootstrap_rejected() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ApiErrorBody {
            error: "registration_not_available",
            message: "Passkey registration is not available for this account.".to_string(),
        }),
    )
        .into_response()
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
    /// Always empty on the bootstrap path -- `resolve_bootstrap_
    /// registration` only ever matches users with none, so there is
    /// nothing to exclude by construction.
    exclude: Vec<Vec<u8>>,
}

pub async fn register_begin(
    State(state): State<AppState>,
    jar: CookieJar,
    Json(request): Json<RegisterBeginRequest>,
) -> Response {
    // Resolved ONCE. Asking twice (a second call to decide
    // `is_bootstrap`) would both waste a round trip and open a window
    // where the two answers disagree -- the path a ceremony is
    // authorized under must be a single decision, not two independent
    // lookups that happen to usually agree.
    let authenticated = try_authenticated_user(&jar, &state).await;
    let is_bootstrap = authenticated.is_none();

    let target = match authenticated {
        Some(user) => match authenticated_target(&state, user.user_id, user.role).await {
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
        },

        None => {
            if !bootstrap_enabled() {
                return bootstrap_rejected();
            }

            let Some(email) = request
                .email
                .as_deref()
                .map(str::trim)
                .filter(|email| !email.is_empty())
            else {
                return bootstrap_rejected();
            };

            match bootstrap_target(&state, email).await {
                Ok(Some(target)) => target,
                Ok(None) => return bootstrap_rejected(),
                Err(err) => {
                    tracing::error!(error = %err, "bootstrap registration lookup failed");
                    return internal_error("Could not start passkey registration");
                }
            }
        }
    };

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

    state
        .registration_ceremonies
        .save(RegistrationCeremony::new(
            ceremony_id.clone(),
            target.user_id,
            challenge.state,
            is_bootstrap,
        ));

    let jar = issue_ceremony_cookie(
        jar,
        REGISTRATION_CEREMONY_COOKIE,
        ceremony_id,
        time::Duration::minutes(CEREMONY_TTL_MINUTES),
    );

    tracing::info!(
        user_id = %target.user_id,
        is_bootstrap,
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
    role: Role,
) -> Result<Option<RegistrationTarget>, sqlx::Error> {
    let mut tx = begin_rls_transaction(&state.db, user_id, role).await?;

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
    }))
}

/// The unauthenticated first-passkey path. Every authorization check
/// lives inside `auth.resolve_bootstrap_registration` (active user, zero
/// existing credentials) -- a `None` here means "not eligible", with the
/// specific reason deliberately not reported back to the caller.
async fn bootstrap_target(
    state: &AppState,
    email: &str,
) -> Result<Option<RegistrationTarget>, sqlx::Error> {
    let row: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT user_id, first_name, last_name FROM auth.resolve_bootstrap_registration($1::citext)",
    )
    .bind(email)
    .fetch_optional(&state.db)
    .await?;

    Ok(
        row.map(|(user_id, first_name, last_name)| RegistrationTarget {
            user_id,
            username: email.to_string(),
            display_name: format!("{first_name} {last_name}"),
            exclude: Vec::new(),
        }),
    )
}

pub async fn register_finish(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(request): Json<RegisterFinishRequest>,
) -> Response {
    let Some(ceremony_id) = read_ceremony_cookie(&jar, REGISTRATION_CEREMONY_COOKIE) else {
        return ceremony_not_found();
    };

    // Read what's needed and release the lock before any `.await` --
    // holding a session lock across an await point is explicitly
    // forbidden by `SessionStore`'s documented locking invariants, and
    // everything below this point is async.
    let Some((user_id, webauthn_state, is_bootstrap)) =
        state
            .registration_ceremonies
            .with_session(&ceremony_id, |ceremony| {
                (
                    ceremony.user_id,
                    ceremony.webauthn_state.clone(),
                    ceremony.is_bootstrap,
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
                error = %err,
                "passkey registration ceremony failed verification"
            );

            return (jar, ceremony_failed()).into_response();
        }
    };

    if let Err(err) = insert_credential(&state, user_id, &stored, request.nickname.as_deref()).await
    {
        tracing::error!(error = %err, user_id = %user_id, "failed to persist passkey credential");
        return (jar, internal_error("Could not save the passkey")).into_response();
    }

    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());

    audit_log::record(
        &state.db,
        audit_log::event::PASSKEY_REGISTERED,
        Some(user_id),
        user_agent,
        serde_json::json!({ "bootstrap": is_bootstrap }),
    )
    .await;

    if !is_bootstrap {
        tracing::info!(user_id = %user_id, "additional passkey registered");

        return (
            jar,
            Json(RegisterFinishResponse {
                success: true,
                session_issued: false,
            }),
        )
            .into_response();
    }

    // Bootstrap only: the entire point of this path is that it ends with
    // the user signed in, since nothing else can issue them a session
    // yet.
    let (raw_token, token_hash) = generate_token();

    let lifetime_hours = session_lifetime_hours();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(lifetime_hours);

    // `ip_address` is left NULL deliberately. Capturing a real client IP
    // needs `into_make_service_with_connect_info` wiring in `main.rs`
    // and -- once this runs behind a proxy -- a trusted-forwarded-header
    // policy that does not exist yet. Recording a wrong or trivially
    // spoofable value in an audit-relevant column is worse than
    // recording none, so it stays null until that decision is actually
    // made.
    let created: Result<Uuid, sqlx::Error> =
        sqlx::query_scalar("SELECT auth.create_session($1, $2, $3, NULL, $4)")
            .bind(user_id)
            .bind(&token_hash)
            .bind(expires_at)
            .bind(user_agent)
            .fetch_one(&state.db)
            .await;

    match created {
        Ok(session_id) => {
            tracing::info!(
                user_id = %user_id,
                session_id = %session_id,
                "bootstrap passkey registered and session issued"
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

/// Writes the verified credential under the target user's own identity.
///
/// Uses the owner-only GUC helper rather than `begin_rls_transaction`:
/// `webauthn_credentials`' RLS policy consults only
/// `app.current_user_id`, and the bootstrap path has no established role
/// to assert anyway -- so setting the role GUC here would hand this
/// write admin visibility it has no use for.
async fn insert_credential(
    state: &AppState,
    user_id: Uuid,
    registered: &RegisteredCredential,
    nickname: Option<&str>,
) -> Result<(), sqlx::Error> {
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

    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::empty_state;

    /// The bootstrap path must stay closed unless explicitly opened.
    /// This is the check standing between "a deliberate one-time setup
    /// step" and "an unauthenticated register-a-passkey-for-this-email
    /// endpoint live in every environment by default".
    #[tokio::test]
    async fn begin_refuses_bootstrap_when_the_env_gate_is_not_set() {
        // Removed rather than assumed absent: this test asserts the
        // DEFAULT, so it must not silently pass or fail based on the
        // developer's own ambient shell environment.
        std::env::remove_var("AUTH_BOOTSTRAP_ENABLED");

        let response = register_begin(
            State(empty_state()),
            CookieJar::new(),
            Json(RegisterBeginRequest {
                email: Some("bmaksimov@quikstor.com".to_string()),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// Even with the gate open, a request carrying no email has nothing
    /// to look up -- and must land on the same indistinguishable
    /// rejection as an ineligible one, not a distinct "missing field"
    /// error that would confirm the gate is open.
    #[tokio::test]
    async fn begin_refuses_bootstrap_without_an_email() {
        let response = register_begin(
            State(empty_state()),
            CookieJar::new(),
            Json(RegisterBeginRequest { email: None }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// No cookie means there is no ceremony to finish -- and critically,
    /// that must not surface as a verification failure or a 500.
    #[tokio::test]
    async fn finish_without_a_ceremony_cookie_is_a_bad_request() {
        let response = register_finish(
            State(empty_state()),
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
                true,
            ));

        let jar = issue_ceremony_cookie(
            CookieJar::new(),
            REGISTRATION_CEREMONY_COOKIE,
            "ceremony-1".to_string(),
            time::Duration::minutes(5),
        );

        let response = register_finish(
            State(state.clone()),
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
