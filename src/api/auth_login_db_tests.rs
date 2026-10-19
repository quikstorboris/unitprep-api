//! Real-database tests for passkey login (`auth_login`): who can start a
//! sign-in, that every ineligible address looks identical, that a ceremony
//! is single use, what a failed assertion leaves behind, and the
//! new-IP/new-device step-up gate.
//!
//! The WebAuthn cryptography is replaced by `ScriptedBackend` (same
//! approach as `auth_register_db_tests`); cookies, the ceremony store, the
//! SQL functions and the RLS role are real.
//!
//! There is deliberately no per-account lockout test here: a passkey
//! assertion is not guessable, so login has no failed-attempt counter. The
//! per-IP rate limit on `/auth/login/*` is covered by
//! `http_integration_tests`, and the TOTP code lockout by
//! `auth_totp_db_tests`.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, State};
use axum::http::{header::SET_COOKIE, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::Json;
use axum_extra::extract::cookie::{Cookie, CookieJar};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::auth_login::{login_begin, login_finish, LoginBeginRequest, LoginFinishRequest};
use super::clickup_db_tests::{body_json, create_user, superuser_pool};
use crate::api::test_support::empty_state;
use crate::api::AppState;
use crate::auth::{
    audit_log, AuthBackend, AuthError, AuthenticationChallenge, AuthenticationOutcome,
    RegisteredCredential, RegistrationChallenge, StoredCredential, LOGIN_CEREMONY_COOKIE,
};

const SESSION_COOKIE_NAME: &str = "unitprep_session";

/// A backend whose assertion result is chosen by the test.
struct ScriptedBackend {
    /// `Some(credential id)` makes `finish_authentication` succeed with it;
    /// `None` makes verification fail.
    accept: Option<Vec<u8>>,
}

impl AuthBackend for ScriptedBackend {
    fn start_registration(
        &self,
        _user_id: Uuid,
        _username: &str,
        _display_name: &str,
        _exclude: &[Vec<u8>],
    ) -> Result<RegistrationChallenge, AuthError> {
        unreachable!("login tests never register")
    }

    fn finish_registration(
        &self,
        _response: serde_json::Value,
        _state: &[u8],
    ) -> Result<RegisteredCredential, AuthError> {
        unreachable!("login tests never register")
    }

    fn start_authentication(
        &self,
        _credentials: &[StoredCredential],
    ) -> Result<AuthenticationChallenge, AuthError> {
        Ok(AuthenticationChallenge {
            challenge: serde_json::json!({ "challenge": "scripted" }),
            state: b"scripted-state".to_vec(),
        })
    }

    fn finish_authentication(
        &self,
        _response: serde_json::Value,
        _state: &[u8],
        _credentials: &[StoredCredential],
    ) -> Result<AuthenticationOutcome, AuthError> {
        match &self.accept {
            Some(id) => Ok(AuthenticationOutcome {
                credential_id: id.clone(),
                updated_passkey_data: serde_json::json!({ "counter": 42 }),
            }),
            None => Err(AuthError::Authentication("scripted failure".to_string())),
        }
    }
}

fn state_with(accept: Option<Vec<u8>>) -> AppState {
    let _ = dotenvy::from_filename(".env.local");
    AppState {
        db: crate::db::connect_test(),
        auth_backend: Arc::new(ScriptedBackend { accept }),
        ..empty_state()
    }
}

fn peer_at(ip: [u8; 4]) -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from((ip, 0)))
}

fn headers_with_agent(agent: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::USER_AGENT,
        HeaderValue::from_str(agent).unwrap(),
    );
    headers
}

/// An active user with one stored passkey; returns the user and the
/// credential id the scripted backend should accept.
async fn user_with_passkey(superuser: &PgPool, label: &str) -> (Uuid, String, Vec<u8>) {
    let id = create_user(superuser, label).await;
    let email: String = sqlx::query_scalar("SELECT email::text FROM auth.users WHERE id = $1")
        .bind(id)
        .fetch_one(superuser)
        .await
        .unwrap();
    let credential_id = Uuid::new_v4().as_bytes().to_vec();
    sqlx::query(
        "INSERT INTO auth.webauthn_credentials (user_id, credential_id, passkey_data)
         VALUES ($1, $2, $3)",
    )
    .bind(id)
    .bind(&credential_id)
    .bind(serde_json::json!({ "counter": 1 }))
    .execute(superuser)
    .await
    .unwrap();
    (id, email, credential_id)
}

async fn begin(state: &AppState, email: &str) -> Response {
    login_begin(
        State(state.clone()),
        peer_at([127, 0, 0, 1]),
        CookieJar::new(),
        HeaderMap::new(),
        Json(LoginBeginRequest {
            email: email.to_string(),
        }),
    )
    .await
}

async fn finish_from(
    state: &AppState,
    jar: CookieJar,
    ip: [u8; 4],
    headers: HeaderMap,
) -> Response {
    login_finish(
        State(state.clone()),
        peer_at(ip),
        jar,
        headers,
        Json(LoginFinishRequest {
            credential: serde_json::json!({ "anything": "the backend decides" }),
        }),
    )
    .await
}

async fn finish(state: &AppState, jar: CookieJar) -> Response {
    finish_from(state, jar, [127, 0, 0, 1], HeaderMap::new()).await
}

fn cookie_named(response: &Response, name: &str) -> Option<Cookie<'static>> {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| Cookie::parse(v.to_string()).ok())
        .find(|c| c.name() == name)
}

fn ceremony_jar(response: &Response) -> CookieJar {
    let cookie =
        cookie_named(response, LOGIN_CEREMONY_COOKIE).expect("begin must set the ceremony cookie");
    CookieJar::new().add(Cookie::new(
        LOGIN_CEREMONY_COOKIE.to_string(),
        cookie.value().to_string(),
    ))
}

async fn error_code(response: Response) -> String {
    body_json(response).await["error"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

async fn audit_count(superuser: &PgPool, event: &str, user: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM auth.auth_audit_logs
          WHERE event_type = $1 AND (actor_user_id = $2 OR target_user_id = $2)",
    )
    .bind(event)
    .bind(user)
    .fetch_one(superuser)
    .await
    .unwrap()
}

async fn audit_reason(superuser: &PgPool, event: &str, user: Uuid) -> Option<String> {
    sqlx::query_scalar(
        "SELECT metadata ->> 'reason' FROM auth.auth_audit_logs
          WHERE event_type = $1 AND actor_user_id = $2 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(event)
    .bind(user)
    .fetch_optional(superuser)
    .await
    .unwrap()
    .flatten()
}

async fn session_count(superuser: &PgPool, user: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM auth.sessions WHERE user_id = $1")
        .bind(user)
        .fetch_one(superuser)
        .await
        .unwrap()
}

// ------------------------------------------------------------- begin

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn login_db_every_ineligible_address_gets_the_same_opaque_401() {
    let superuser = superuser_pool();
    let state = state_with(None);

    // Active, but no passkey enrolled.
    let no_passkey = create_user(&superuser, "no-passkey").await;
    let no_passkey_email: String =
        sqlx::query_scalar("SELECT email::text FROM auth.users WHERE id = $1")
            .bind(no_passkey)
            .fetch_one(&superuser)
            .await
            .unwrap();

    // Has a passkey, but the account was deactivated.
    let (deactivated, deactivated_email, _) = user_with_passkey(&superuser, "deactivated").await;
    sqlx::query("SELECT auth.set_user_status($1, 'deactivated'::auth.user_status)")
        .bind(deactivated)
        .execute(&superuser)
        .await
        .unwrap();

    let unknown_email = format!("nobody-{}@example.test", Uuid::new_v4());

    let mut bodies = Vec::new();
    for email in [
        unknown_email.as_str(),
        no_passkey_email.as_str(),
        deactivated_email.as_str(),
        "   ",
    ] {
        let response = begin(&state, email).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "for {email:?}");
        assert!(
            cookie_named(&response, LOGIN_CEREMONY_COOKIE).is_none(),
            "an ineligible address must not receive a ceremony"
        );
        bodies.push(body_json(response).await);
    }
    assert!(
        bodies.windows(2).all(|pair| pair[0] == pair[1]),
        "responses must be indistinguishable, or login is a user-enumeration oracle: {bodies:?}"
    );

    // The probe itself is still recorded, with the address in metadata.
    let probed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM auth.auth_audit_logs
          WHERE event_type = $1 AND metadata ->> 'email' = $2",
    )
    .bind(audit_log::event::LOGIN_FAILED)
    .bind(&unknown_email)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert_eq!(probed, 1);
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn login_db_an_eligible_address_gets_a_challenge_and_a_ceremony_cookie() {
    let superuser = superuser_pool();
    let (_, email, credential_id) = user_with_passkey(&superuser, "eligible").await;
    let state = state_with(Some(credential_id));

    let response = begin(&state, &format!("  {}  ", email.to_uppercase())).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(cookie_named(&response, LOGIN_CEREMONY_COOKIE).is_some());
    assert_eq!(
        body_json(response).await["challenge"]["challenge"],
        "scripted"
    );
}

// ------------------------------------------------------------ finish

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn login_db_a_verified_assertion_issues_a_session_persists_the_counter_and_audits() {
    let superuser = superuser_pool();
    let (user, email, credential_id) = user_with_passkey(&superuser, "signer").await;
    let state = state_with(Some(credential_id.clone()));
    let jar = ceremony_jar(&begin(&state, &email).await);

    let response = finish(&state, jar).await;

    assert_eq!(response.status(), StatusCode::OK);
    let session_cookie = cookie_named(&response, SESSION_COOKIE_NAME).expect("session cookie");
    assert!(!session_cookie.value().is_empty());
    assert!(
        cookie_named(&response, LOGIN_CEREMONY_COOKIE)
            .map(|c| c.value().is_empty())
            .unwrap_or(true),
        "the ceremony cookie must be cleared on finish"
    );
    let body = body_json(response).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["step_up_required"], false);

    assert_eq!(session_count(&superuser, user).await, 1);
    assert_eq!(
        audit_count(&superuser, audit_log::event::LOGIN_SUCCEEDED, user).await,
        1
    );

    // The anti-cloning counter webauthn-rs advanced must be persisted.
    let (data, stamped): (Value, bool) = sqlx::query_as(
        "SELECT passkey_data, last_used_at IS NOT NULL FROM auth.webauthn_credentials
          WHERE user_id = $1 AND credential_id = $2",
    )
    .bind(user)
    .bind(&credential_id)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert_eq!(data["counter"], 42);
    assert!(stamped);
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn login_db_a_rejected_assertion_issues_no_session_and_burns_the_ceremony() {
    let superuser = superuser_pool();
    let (user, email, _) = user_with_passkey(&superuser, "rejected").await;
    let state = state_with(None);
    let jar = ceremony_jar(&begin(&state, &email).await);

    let response = finish(&state, jar.clone()).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(cookie_named(&response, SESSION_COOKIE_NAME).is_none());
    assert_eq!(error_code(response).await, "login_failed");
    assert_eq!(session_count(&superuser, user).await, 0);
    assert_eq!(
        audit_reason(&superuser, audit_log::event::LOGIN_FAILED, user)
            .await
            .as_deref(),
        Some("assertion_rejected")
    );

    // Single use: replaying the same cookie against the same ceremony gets
    // no second try.
    let replay = finish(&state, jar).await;
    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(replay).await, "ceremony_not_found");
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn login_db_a_successful_ceremony_cannot_be_replayed_for_a_second_session() {
    let superuser = superuser_pool();
    let (user, email, credential_id) = user_with_passkey(&superuser, "replayer").await;
    let state = state_with(Some(credential_id));
    let jar = ceremony_jar(&begin(&state, &email).await);
    assert_eq!(finish(&state, jar.clone()).await.status(), StatusCode::OK);

    let replay = finish(&state, jar).await;

    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    assert_eq!(session_count(&superuser, user).await, 1);
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn login_db_passkeys_removed_mid_ceremony_fail_the_login_and_are_audited() {
    let superuser = superuser_pool();
    let (user, email, credential_id) = user_with_passkey(&superuser, "vanishing").await;
    let state = state_with(Some(credential_id));
    let jar = ceremony_jar(&begin(&state, &email).await);

    // e.g. an admin deactivated or recovered the account in between.
    sqlx::query("DELETE FROM auth.webauthn_credentials WHERE user_id = $1")
        .bind(user)
        .execute(&superuser)
        .await
        .unwrap();

    let response = finish(&state, jar).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(session_count(&superuser, user).await, 0);
    assert_eq!(
        audit_reason(&superuser, audit_log::event::LOGIN_FAILED, user)
            .await
            .as_deref(),
        Some("credentials_removed_mid_ceremony")
    );
}

// --------------------------------------------------------- anomaly gate

/// Seeds a prior session from `ip` / `agent`, so the next login has history
/// to be compared against.
async fn prior_session(superuser: &PgPool, user: Uuid, ip: &str, agent: &str) {
    let (_, token_hash) = crate::auth::generate_token();
    sqlx::query(
        "INSERT INTO auth.sessions (user_id, token_hash, expires_at, ip_address, user_agent)
         VALUES ($1, $2, now() + interval '1 hour', $3::inet, $4)",
    )
    .bind(user)
    .bind(&token_hash)
    .bind(ip)
    .bind(agent)
    .execute(superuser)
    .await
    .unwrap();
}

async fn confirm_totp(superuser: &PgPool, user: Uuid) {
    sqlx::query(
        "INSERT INTO auth.totp_credentials (user_id, secret_encrypted, confirmed_at)
         VALUES ($1, $2, now())",
    )
    .bind(user)
    .bind(vec![1u8, 2, 3])
    .execute(superuser)
    .await
    .unwrap();
}

async fn newest_session_requires_step_up(superuser: &PgPool, user: Uuid) -> bool {
    sqlx::query_scalar(
        "SELECT requires_step_up FROM auth.sessions WHERE user_id = $1
          ORDER BY created_at DESC LIMIT 1",
    )
    .bind(user)
    .fetch_one(superuser)
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn login_db_a_login_from_a_new_ip_and_device_is_gated_when_totp_is_confirmed() {
    let superuser = superuser_pool();
    let (user, email, credential_id) = user_with_passkey(&superuser, "traveller").await;
    prior_session(&superuser, user, "203.0.113.5", "HomeBrowser/1.0").await;
    confirm_totp(&superuser, user).await;
    let state = state_with(Some(credential_id));
    let jar = ceremony_jar(&begin(&state, &email).await);

    let response = finish_from(
        &state,
        jar,
        [198, 51, 100, 9],
        headers_with_agent("CafeBrowser/2.0"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["step_up_required"], true);
    assert!(newest_session_requires_step_up(&superuser, user).await);
    assert_eq!(
        audit_count(&superuser, audit_log::event::LOGIN_ANOMALY_DETECTED, user).await,
        1
    );
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn login_db_an_anomalous_login_without_totp_is_audited_but_not_locked_out() {
    let superuser = superuser_pool();
    let (user, email, credential_id) = user_with_passkey(&superuser, "no-factor").await;
    prior_session(&superuser, user, "203.0.113.5", "HomeBrowser/1.0").await;
    let state = state_with(Some(credential_id));
    let jar = ceremony_jar(&begin(&state, &email).await);

    let response = finish_from(
        &state,
        jar,
        [198, 51, 100, 9],
        headers_with_agent("CafeBrowser/2.0"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["step_up_required"], false);
    assert!(!newest_session_requires_step_up(&superuser, user).await);
    assert_eq!(
        audit_count(&superuser, audit_log::event::LOGIN_ANOMALY_DETECTED, user).await,
        1,
        "forcing a step-up with no factor would be a lockout, but the anomaly is still recorded"
    );
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn login_db_a_familiar_ip_or_device_is_not_anomalous_and_a_first_login_never_is() {
    let superuser = superuser_pool();

    // Same IP as a prior session, different device: familiar enough.
    let (user, email, credential_id) = user_with_passkey(&superuser, "regular").await;
    prior_session(&superuser, user, "198.51.100.9", "HomeBrowser/1.0").await;
    confirm_totp(&superuser, user).await;
    let state = state_with(Some(credential_id));
    let jar = ceremony_jar(&begin(&state, &email).await);
    let response = finish_from(
        &state,
        jar,
        [198, 51, 100, 9],
        headers_with_agent("NewLaptopBrowser/3.0"),
    )
    .await;
    assert_eq!(body_json(response).await["step_up_required"], false);
    assert_eq!(
        audit_count(&superuser, audit_log::event::LOGIN_ANOMALY_DETECTED, user).await,
        0
    );

    // A brand-new account has nothing to compare against.
    let (fresh, fresh_email, fresh_credential) = user_with_passkey(&superuser, "first").await;
    confirm_totp(&superuser, fresh).await;
    let state = state_with(Some(fresh_credential));
    let jar = ceremony_jar(&begin(&state, &fresh_email).await);
    let response = finish_from(
        &state,
        jar,
        [192, 0, 2, 77],
        headers_with_agent("AnyBrowser/1.0"),
    )
    .await;
    assert_eq!(body_json(response).await["step_up_required"], false);
    assert_eq!(
        audit_count(&superuser, audit_log::event::LOGIN_ANOMALY_DETECTED, fresh).await,
        0
    );
}
