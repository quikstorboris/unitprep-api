//! WebAuthn passkey login, HTTP side (Phase 2 task 5). Mirrors
//! `auth_register` in shape: `AuthBackend` does the cryptography, this
//! module does the orchestration -- finding the caller's credentials,
//! holding ceremony state across the two requests a WebAuthn ceremony
//! needs, writing back what the authenticator changed, and issuing the
//! session.
//!
//! ## The two legs need different database access, and that is the whole
//! ## interesting part
//!
//! `/begin` starts from a client-supplied email and nothing else. There is
//! no session, so no `app.current_user_id` GUC -- and
//! `webauthn_credentials` is guarded by an owner-only policy keyed on
//! exactly that setting, so reading the table directly would return zero
//! rows for everyone and make login structurally impossible. Hence
//! `auth.resolve_login_candidate`, a SECURITY DEFINER lookup.
//!
//! `/finish` does NOT need that bypass. By then the ceremony holds a
//! `user_id` that was put there server-side and never came from the
//! client, so this handler can set the GUC from it and read *and* write
//! the same rows through ordinary owner-scoped RLS.
//!
//! ## What login writes
//!
//! Verifying an assertion mutates the stored credential: webauthn-rs bumps
//! its internal signature counter, which is the anti-cloning mechanism, so
//! the updated blob has to be persisted or the protection silently stops
//! working. `last_used_at` is stamped in the same statement.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use unitprep_core::session_store::SessionStoreExt;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{
    audit_log, begin_owner_rls_transaction, clear_ceremony_cookie, generate_token,
    issue_ceremony_cookie, issue_session_cookie, read_ceremony_cookie, AuthenticationCeremony,
    StoredCredential, LOGIN_CEREMONY_COOKIE,
};

/// Matches the registration ceremony's TTL and the ceremony store's own
/// timeout in `main.rs` -- see the note there on why this is fixed rather
/// than env-tunable.
const CEREMONY_TTL_MINUTES: i64 = 5;

/// Same override and same default as the bootstrap-registration path, so a
/// session's lifetime does not depend on which endpoint minted it.
fn session_lifetime_hours() -> i64 {
    std::env::var("SESSION_LIFETIME_HOURS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|hours| *hours > 0)
        .unwrap_or(12)
}

#[derive(Debug, Deserialize)]
pub struct LoginBeginRequest {
    pub email: String,
}

#[derive(Debug, Serialize)]
pub struct LoginBeginResponse {
    /// Passed straight to `navigator.credentials.get()` by the frontend.
    pub challenge: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct LoginFinishRequest {
    /// Exactly what `navigator.credentials.get()` resolved to, relayed
    /// unmodified. Verified by the backend against the stored ceremony
    /// state; never trusted here.
    pub credential: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct LoginFinishResponse {
    pub success: bool,
    /// True when this login was flagged as anomalous (new IP or user_agent
    /// for an account with prior sessions) and the account has TOTP
    /// confirmed -- every route except step-up itself and whoami will 403
    /// until the frontend prompts for and submits a code. See
    /// `AuthenticatedUser`'s `STEP_UP_ALLOWED_PATHS`.
    pub step_up_required: bool,
}

/// One response for every reason a login cannot start: unknown email,
/// inactive user, soft-deleted user, or an active user with no passkey
/// enrolled. `auth.resolve_login_candidate` returns zero rows for all
/// four and this handler cannot tell them apart either -- which is the
/// point. Telling them apart would make this unauthenticated endpoint a
/// user-enumeration oracle.
///
/// A 401 rather than a 404: the caller is not authenticated, and "no
/// account like that" and "that account cannot use this method" should not
/// be separable by status code any more than by message.
fn login_unavailable() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ApiErrorBody {
            error: "login_unavailable",
            message: "Could not sign in with that address.".to_string(),
        }),
    )
        .into_response()
}

fn ceremony_not_found() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "ceremony_not_found",
            message: "This sign-in attempt has expired or was never started. Start again."
                .to_string(),
        }),
    )
        .into_response()
}

fn ceremony_failed() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ApiErrorBody {
            error: "login_failed",
            message: "That passkey could not be verified.".to_string(),
        }),
    )
        .into_response()
}

pub async fn login_begin(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(request): Json<LoginBeginRequest>,
) -> Response {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());

    let email = request.email.trim();

    if email.is_empty() {
        // No actor, for the same reason the lookup-came-back-empty case
        // just below has none -- and recorded for the same reason that one
        // is: an empty address is exactly as much "no usable credential"
        // as one that fails to resolve, and leaving this specific shape of
        // the same probe unaudited would be an asymmetry, not a decision.
        audit_log::record(
            &state.db,
            audit_log::event::LOGIN_FAILED,
            audit_log::Subjects::anonymous(),
            user_agent,
            None,
            audit_log::Change::none(),
            serde_json::json!({ "reason": "empty_email" }),
        )
        .await;

        return login_unavailable();
    }

    let credentials = match load_login_candidate(&state, email).await {
        Ok(Some(loaded)) => loaded,
        Ok(None) => {
            // Logged with no actor_user_id: there may be no user at all
            // behind this address. The email goes in metadata rather than
            // a column precisely because it may not correspond to a real
            // account -- an unmatched address is data about the attempt,
            // not an identity.
            audit_log::record(
                &state.db,
                audit_log::event::LOGIN_FAILED,
                audit_log::Subjects::anonymous(),
                user_agent,
                None,
                audit_log::Change::none(),
                serde_json::json!({ "reason": "no_usable_credential", "email": email }),
            )
            .await;

            return login_unavailable();
        }
        Err(err) => {
            tracing::error!(error = %err, "login candidate lookup failed");
            return internal_error("Could not start sign-in");
        }
    };

    let challenge = match state.auth_backend.start_authentication(&credentials.stored) {
        Ok(challenge) => challenge,
        Err(err) => {
            tracing::error!(error = %err, "failed to start passkey authentication ceremony");
            return internal_error("Could not start sign-in");
        }
    };

    let ceremony_id = Uuid::new_v4().to_string();

    let ceremony =
        AuthenticationCeremony::new(ceremony_id.clone(), credentials.user_id, challenge.state);

    // Read before the store takes ownership. Logged in place of
    // `ceremony_id`, which is the cookie's own value -- see
    // `AuthenticationCeremony::correlation_id`.
    let correlation_id = ceremony.correlation_id;

    state.authentication_ceremonies.save(ceremony);

    let jar = issue_ceremony_cookie(
        jar,
        LOGIN_CEREMONY_COOKIE,
        ceremony_id,
        time::Duration::minutes(CEREMONY_TTL_MINUTES),
    );

    tracing::info!(
        user_id = %credentials.user_id,
        correlation_id = %correlation_id,
        "passkey login ceremony started"
    );

    (
        jar,
        Json(LoginBeginResponse {
            challenge: challenge.challenge,
        }),
    )
        .into_response()
}

struct LoginCandidate {
    user_id: Uuid,
    stored: Vec<StoredCredential>,
}

/// Resolves an email to its owner's credential set through the SECURITY
/// DEFINER lookup. `Ok(None)` means "not eligible", with the reason
/// deliberately unavailable to the caller.
async fn load_login_candidate(
    state: &AppState,
    email: &str,
) -> Result<Option<LoginCandidate>, sqlx::Error> {
    let rows: Vec<(Uuid, Vec<u8>, serde_json::Value)> = sqlx::query_as(
        "SELECT user_id, credential_id, passkey_data FROM auth.resolve_login_candidate($1::citext)",
    )
    .bind(email)
    .fetch_all(&state.db)
    .await?;

    // One row per credential, so the user_id repeats. Zero rows is the
    // ineligible case.
    let Some((user_id, _, _)) = rows.first() else {
        return Ok(None);
    };

    let user_id = *user_id;

    let stored = rows
        .into_iter()
        .map(|(_, credential_id, passkey_data)| StoredCredential {
            credential_id,
            passkey_data,
        })
        .collect();

    Ok(Some(LoginCandidate { user_id, stored }))
}

pub async fn login_finish(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(request): Json<LoginFinishRequest>,
) -> Response {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());

    let Some(ceremony_id) = read_ceremony_cookie(&jar, LOGIN_CEREMONY_COOKIE) else {
        return ceremony_not_found();
    };

    // Read and release before any `.await` -- holding a session lock
    // across an await point is forbidden by `SessionStore`'s documented
    // locking invariants.
    let Some((user_id, correlation_id, webauthn_state)) = state
        .authentication_ceremonies
        .with_session(&ceremony_id, |ceremony| {
            (
                ceremony.user_id,
                ceremony.correlation_id,
                ceremony.webauthn_state.clone(),
            )
        })
    else {
        return (
            clear_ceremony_cookie(jar, LOGIN_CEREMONY_COOKIE),
            ceremony_not_found(),
        )
            .into_response();
    };

    // Single-use, consumed BEFORE verification: a failed or replayed
    // attempt must not get a second try against the same challenge. This
    // matters more here than for registration -- an attacker replaying
    // assertions against a live challenge is a real attack, not just an
    // untidy retry.
    state.authentication_ceremonies.delete(&ceremony_id);

    let jar = clear_ceremony_cookie(jar, LOGIN_CEREMONY_COOKIE);

    // Re-read the credential set under the ceremony's own identity rather
    // than trusting a copy taken at `/begin` -- see AuthenticationCeremony
    // on why the set is not carried in the ceremony.
    let stored = match load_credentials_for_user(&state, user_id).await {
        Ok(stored) if !stored.is_empty() => stored,
        Ok(_) => {
            // Every passkey was removed mid-ceremony. Not an error state,
            // just no longer authenticable.
            audit_log::record(
                &state.db,
                audit_log::event::LOGIN_FAILED,
                audit_log::Subjects::by(user_id),
                user_agent,
                None,
                audit_log::Change::none(),
                serde_json::json!({
                    "reason": "credentials_removed_mid_ceremony",
                    "correlation_id": correlation_id,
                }),
            )
            .await;

            return (jar, ceremony_failed()).into_response();
        }
        Err(err) => {
            tracing::error!(
                error = %err,
                user_id = %user_id,
                correlation_id = %correlation_id,
                "failed to reload credentials"
            );
            return (jar, internal_error("Could not complete sign-in")).into_response();
        }
    };

    let outcome =
        match state
            .auth_backend
            .finish_authentication(request.credential, &webauthn_state, &stored)
        {
            Ok(outcome) => outcome,
            Err(err) => {
                // `warn`, not `error`: a failed assertion is an ordinary
                // client-side outcome (wrong device, cancelled prompt, stale
                // challenge), not a server fault.
                tracing::warn!(
                    user_id = %user_id,
                    correlation_id = %correlation_id,
                    error = %err,
                    "passkey login ceremony failed verification"
                );

                audit_log::record(
                    &state.db,
                    audit_log::event::LOGIN_FAILED,
                    audit_log::Subjects::by(user_id),
                    user_agent,
                    None,
                    audit_log::Change::none(),
                    serde_json::json!({
                        "reason": "assertion_rejected",
                        "correlation_id": correlation_id,
                    }),
                )
                .await;

                return (jar, ceremony_failed()).into_response();
            }
        };

    // The signature counter webauthn-rs just advanced is the anti-cloning
    // mechanism; not persisting it would leave the check running against a
    // frozen value, which passes forever and detects nothing.
    if let Err(err) = persist_credential_use(&state, user_id, &outcome).await {
        tracing::error!(
            error = %err,
            user_id = %user_id,
            correlation_id = %correlation_id,
            "failed to persist post-authentication credential state"
        );
        return (jar, internal_error("Could not complete sign-in")).into_response();
    }

    let (raw_token, token_hash) = generate_token();
    let lifetime_hours = session_lifetime_hours();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(lifetime_hours);
    let client_ip = sqlx::types::ipnetwork::IpNetwork::from(addr.ip());

    // Phase II anomaly signal: flag a login from an IP or user_agent never
    // seen before for an account that has *some* session history (a brand
    // new account's very first login has nothing to compare against, so it
    // is never anomalous). See assess_login_risk's own docs for the query
    // and the reasoning behind auditing unconditionally but gating only
    // when TOTP is confirmed.
    let risk = match assess_login_risk(&state, user_id, client_ip, user_agent).await {
        Ok(risk) => risk,
        Err(err) => {
            tracing::error!(
                error = %err,
                user_id = %user_id,
                correlation_id = %correlation_id,
                "failed to assess login risk"
            );
            return (jar, internal_error("Could not complete sign-in")).into_response();
        }
    };

    if risk.is_anomalous {
        audit_log::record(
            &state.db,
            audit_log::event::LOGIN_ANOMALY_DETECTED,
            audit_log::Subjects::by(user_id),
            user_agent,
            Some(client_ip),
            audit_log::Change::none(),
            serde_json::json!({
                "correlation_id": correlation_id,
                "step_up_required": risk.totp_confirmed,
            }),
        )
        .await;
    }

    let requires_step_up = risk.is_anomalous && risk.totp_confirmed;

    let created: Result<Uuid, sqlx::Error> =
        sqlx::query_scalar("SELECT auth.create_session($1, $2, $3, $4, $5, $6)")
            .bind(user_id)
            .bind(&token_hash)
            .bind(expires_at)
            .bind(client_ip)
            .bind(user_agent)
            .bind(requires_step_up)
            .fetch_one(&state.db)
            .await;

    match created {
        Ok(session_id) => {
            audit_log::record(
                &state.db,
                audit_log::event::LOGIN_SUCCEEDED,
                audit_log::Subjects::by(user_id),
                user_agent,
                Some(client_ip),
                audit_log::Change::none(),
                serde_json::json!({
                    "session_id": session_id,
                    "correlation_id": correlation_id,
                }),
            )
            .await;

            tracing::info!(
                user_id = %user_id,
                session_id = %session_id,
                correlation_id = %correlation_id,
                requires_step_up,
                "passkey login succeeded"
            );

            let jar = issue_session_cookie(jar, raw_token, time::Duration::hours(lifetime_hours));

            (
                jar,
                Json(LoginFinishResponse {
                    success: true,
                    step_up_required: requires_step_up,
                }),
            )
                .into_response()
        }
        Err(err) => {
            // The assertion verified, so this is not a failed login -- the
            // credential is genuine and its counter has already been
            // advanced. Only session creation failed, which is a server
            // fault.
            tracing::error!(
                error = %err,
                user_id = %user_id,
                correlation_id = %correlation_id,
                "passkey verified but session creation failed"
            );

            (
                jar,
                internal_error("Signed in, but the session could not be created"),
            )
                .into_response()
        }
    }
}

/// What `assess_login_risk` found. `is_anomalous` and `totp_confirmed` are
/// deliberately separate rather than pre-combined into one "should we gate
/// this" bool -- the caller audits on the first and only gates on the
/// pairing of both, and it needs `totp_confirmed` on its own to decide
/// whether the audit metadata should say the account was left ungated for
/// lack of a step-up factor.
struct LoginRiskAssessment {
    /// This account has at least one prior session, and this login's IP
    /// and user_agent both differ from every one of them. A brand new
    /// account's very first login is never anomalous under this
    /// definition -- there is nothing yet to compare against.
    is_anomalous: bool,
    /// Whether this account has a confirmed TOTP credential to step up
    /// with at all. An anomalous login on an account without one is
    /// audited but never gated -- forcing a step-up with no factor to
    /// satisfy it would be a lockout, not a hardening measure.
    totp_confirmed: bool,
}

/// Assesses the Phase II anomaly signal for a login that has already
/// cryptographically verified. Read-only, under the user's own identity
/// (safe the same way `load_credentials_for_user` is: `user_id` comes from
/// server-side ceremony state, never from the request) -- no SECURITY
/// DEFINER bypass needed, since `sessions_select_own_or_admin` already lets
/// an owner read their own session history.
///
/// "Unexpected location" is scoped down to "new IP address" rather than
/// true geolocation -- see the migration's own docs for why. A NULL
/// `ip_address` on a historical row (every row created before this
/// feature, and any future row from a client whose peer address somehow
/// isn't available) can never equal a real IP in SQL, so old rows simply
/// don't count as "seen" for IP purposes, which is the correct
/// fail-safe direction: it can make a login look newer than it is, never
/// hide a genuinely new one.
async fn assess_login_risk(
    state: &AppState,
    user_id: Uuid,
    ip_address: sqlx::types::ipnetwork::IpNetwork,
    user_agent: Option<&str>,
) -> Result<LoginRiskAssessment, sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(&state.db, user_id).await?;

    let (has_history, seen_ip, seen_user_agent): (bool, bool, bool) = sqlx::query_as(
        "SELECT
            EXISTS(SELECT 1 FROM auth.sessions WHERE user_id = $1),
            EXISTS(SELECT 1 FROM auth.sessions WHERE user_id = $1 AND ip_address = $2),
            EXISTS(SELECT 1 FROM auth.sessions WHERE user_id = $1 AND user_agent = $3)",
    )
    .bind(user_id)
    .bind(ip_address)
    .bind(user_agent)
    .fetch_one(&mut *tx)
    .await?;

    let totp_confirmed: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM auth.totp_credentials
             WHERE user_id = $1 AND confirmed_at IS NOT NULL
         )",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(LoginRiskAssessment {
        is_anomalous: has_history && !(seen_ip || seen_user_agent),
        totp_confirmed,
    })
}

/// Reads a user's credentials under their own identity. Safe to scope this
/// way because `user_id` comes from server-side ceremony state, never from
/// the request.
async fn load_credentials_for_user(
    state: &AppState,
    user_id: Uuid,
) -> Result<Vec<StoredCredential>, sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(&state.db, user_id).await?;

    let rows: Vec<(Vec<u8>, serde_json::Value)> = sqlx::query_as(
        "SELECT credential_id, passkey_data FROM auth.webauthn_credentials WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(rows
        .into_iter()
        .map(|(credential_id, passkey_data)| StoredCredential {
            credential_id,
            passkey_data,
        })
        .collect())
}

/// Writes back the credential state the ceremony advanced, plus the
/// last-used stamp. Scoped by `credential_id` as well as `user_id` so a
/// backend that somehow reported a credential belonging to someone else
/// updates nothing rather than the wrong row.
async fn persist_credential_use(
    state: &AppState,
    user_id: Uuid,
    outcome: &crate::auth::AuthenticationOutcome,
) -> Result<(), sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(&state.db, user_id).await?;

    sqlx::query(
        "UPDATE auth.webauthn_credentials
            SET passkey_data = $1, last_used_at = now()
          WHERE user_id = $2 AND credential_id = $3",
    )
    .bind(&outcome.updated_passkey_data)
    .bind(user_id)
    .bind(&outcome.credential_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::empty_state;

    /// A stand-in peer address for tests -- `login_finish` now takes
    /// `ConnectInfo<SocketAddr>` (only populated for real by
    /// `into_make_service_with_connect_info` outside of tests), so every
    /// direct call needs one. The actual value never matters here: none of
    /// these tests reach `assess_login_risk` (they all fail earlier, at
    /// ceremony lookup or credential reload against the unreachable test
    /// pool).
    fn test_addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    /// An empty address must not reach the database, and must land on the
    /// same indistinguishable rejection as any other ineligible input.
    #[tokio::test]
    async fn begin_refuses_an_empty_email_without_touching_the_database() {
        // `empty_state`'s pool is lazy and points at nothing reachable, so
        // this test also proves no query was attempted -- a query would
        // surface as a 500, not a 401.
        let response = login_begin(
            State(empty_state()),
            CookieJar::new(),
            HeaderMap::new(),
            Json(LoginBeginRequest {
                email: "   ".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// No cookie means there is no ceremony to finish -- and that must not
    /// surface as a verification failure or a 500.
    #[tokio::test]
    async fn finish_without_a_ceremony_cookie_is_a_bad_request() {
        let response = login_finish(
            State(empty_state()),
            test_addr(),
            CookieJar::new(),
            HeaderMap::new(),
            Json(LoginFinishRequest {
                credential: serde_json::json!({}),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// A cookie naming a ceremony the store never had (expired, process
    /// restarted, fabricated) is the same "start again" case as no cookie.
    #[tokio::test]
    async fn finish_with_an_unknown_ceremony_id_is_a_bad_request() {
        let jar = issue_ceremony_cookie(
            CookieJar::new(),
            LOGIN_CEREMONY_COOKIE,
            "not-a-real-ceremony".to_string(),
            time::Duration::minutes(5),
        );

        let response = login_finish(
            State(empty_state()),
            test_addr(),
            jar,
            HeaderMap::new(),
            Json(LoginFinishRequest {
                credential: serde_json::json!({}),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// Regression guard for the single-use property, written against a
    /// FAILING attempt because that is the case a "delete on success"
    /// implementation gets wrong while looking correct in the happy path.
    /// Replaying a live assertion challenge is a real attack, so this
    /// matters more here than on the registration path.
    #[tokio::test]
    async fn a_failed_finish_still_consumes_the_ceremony() {
        let state = empty_state();
        let user_id = Uuid::new_v4();

        state
            .authentication_ceremonies
            .save(AuthenticationCeremony::new(
                "login-ceremony-1".to_string(),
                user_id,
                b"not-valid-webauthn-state".to_vec(),
            ));

        let jar = issue_ceremony_cookie(
            CookieJar::new(),
            LOGIN_CEREMONY_COOKIE,
            "login-ceremony-1".to_string(),
            time::Duration::minutes(5),
        );

        let response = login_finish(
            State(state.clone()),
            test_addr(),
            jar,
            HeaderMap::new(),
            Json(LoginFinishRequest {
                credential: serde_json::json!({ "garbage": true }),
            }),
        )
        .await;

        // The credential reload hits an unreachable pool, so this fails
        // before verification -- but the ceremony must already be gone
        // either way. That is exactly the property under test: consumption
        // happens before anything that can fail, not after success.
        assert!(
            state
                .authentication_ceremonies
                .get_handle("login-ceremony-1")
                .is_none(),
            "a consumed ceremony must not remain in the store"
        );

        assert_ne!(
            response.status(),
            StatusCode::OK,
            "a login against nonsense ceremony state must never succeed"
        );
    }

    /// A registration ceremony cookie must not be accepted by the login
    /// finish endpoint. Guards the separate-cookie-names decision from
    /// being quietly collapsed later.
    #[tokio::test]
    async fn finish_ignores_a_registration_ceremony_cookie() {
        use crate::auth::REGISTRATION_CEREMONY_COOKIE;

        let state = empty_state();
        let jar = issue_ceremony_cookie(
            CookieJar::new(),
            REGISTRATION_CEREMONY_COOKIE,
            "reg-ceremony".to_string(),
            time::Duration::minutes(5),
        );

        let response = login_finish(
            State(state),
            test_addr(),
            jar,
            HeaderMap::new(),
            Json(LoginFinishRequest {
                credential: serde_json::json!({}),
            }),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "a registration ceremony cookie must read as no login ceremony at all"
        );
    }
}
