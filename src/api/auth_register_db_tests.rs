//! Real-database characterization tests for passkey registration
//! (`register_begin` / `register_finish`), written BEFORE the file is split
//! (efficiency refactor D4g) so the split has a safety net around the
//! security-sensitive paths its module doc promises:
//!
//! * the invite is consumed in the SAME transaction as the credential
//!   insert, so the only reachable outcomes are "enrolled and active" or
//!   "untouched and retryable";
//! * a failed ceremony leaves the invite usable;
//! * an authenticated caller registers a passkey for THEMSELVES only.
//!
//! The WebAuthn cryptography is replaced by `ScriptedBackend`; everything
//! else (cookies, ceremony store, SQL functions, RLS role) is real. Every
//! test is `#[ignore]`d -- local `test-db` only; see `clickup_db_tests`'
//! module doc for how to run them.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, State};
use axum::http::{header::SET_COOKIE, HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use axum_extra::extract::cookie::{Cookie, CookieJar};
use sqlx::PgPool;
use uuid::Uuid;

use super::auth_register::{
    register_begin, register_finish, RegisterBeginRequest, RegisterFinishRequest,
};
use super::clickup_db_tests::{body_json, create_user, superuser_pool};
use crate::api::test_support::empty_state;
use crate::api::AppState;
use crate::auth::{
    hash_token, AuthBackend, AuthError, AuthenticationChallenge, AuthenticationOutcome,
    RegisteredCredential, RegistrationChallenge, StoredCredential, REGISTRATION_CEREMONY_COOKIE,
};

/// The session cookie's name --  keeps it private on purpose, so the
/// tests spell it out (a rename would break every signed-in request first).
const SESSION_COOKIE_NAME: &str = "unitprep_session";

/// A backend whose registration result is chosen by the test.
struct ScriptedBackend {
    /// `Some(credential id)` makes `finish_registration` succeed with it;
    /// `None` makes it fail verification.
    finish_with: Option<Vec<u8>>,
}

impl AuthBackend for ScriptedBackend {
    fn start_registration(
        &self,
        _user_id: Uuid,
        _username: &str,
        _display_name: &str,
        _exclude: &[Vec<u8>],
    ) -> Result<RegistrationChallenge, AuthError> {
        Ok(RegistrationChallenge {
            challenge: serde_json::json!({ "challenge": "scripted" }),
            state: b"scripted-state".to_vec(),
        })
    }

    fn finish_registration(
        &self,
        _response: serde_json::Value,
        _state: &[u8],
    ) -> Result<RegisteredCredential, AuthError> {
        match &self.finish_with {
            Some(id) => Ok(RegisteredCredential {
                credential_id: id.clone(),
                passkey_data: serde_json::json!({ "scripted": true }),
                device_bound: false,
            }),
            None => Err(AuthError::Registration("scripted failure".to_string())),
        }
    }

    fn start_authentication(
        &self,
        _credentials: &[StoredCredential],
    ) -> Result<AuthenticationChallenge, AuthError> {
        unreachable!("registration tests never authenticate")
    }

    fn finish_authentication(
        &self,
        _response: serde_json::Value,
        _state: &[u8],
        _credentials: &[StoredCredential],
    ) -> Result<AuthenticationOutcome, AuthError> {
        unreachable!("registration tests never authenticate")
    }
}

fn state_with(finish_with: Option<Vec<u8>>) -> AppState {
    AppState {
        db: crate::db::connect_test(),
        auth_backend: Arc::new(ScriptedBackend { finish_with }),
        ..empty_state()
    }
}

fn peer() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
}

/// A user in `invited` status plus a live invite for `raw_token`.
async fn invited_user(superuser: &PgPool, raw_token: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO auth.users (id, email, first_name, last_name, company, status)
         VALUES ($1, $2, 'Invited', 'Person', 'quikstor', 'invited')",
    )
    .bind(id)
    .bind(format!("invited-{id}@example.test"))
    .execute(superuser)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO auth.user_invites (user_id, token_hash, expires_at)
         VALUES ($1, $2, now() + interval '1 hour')",
    )
    .bind(id)
    .bind(hash_token(raw_token))
    .execute(superuser)
    .await
    .unwrap();
    id
}

fn invite_request(raw_token: &str) -> Json<RegisterBeginRequest> {
    Json(RegisterBeginRequest {
        invite_token: Some(raw_token.to_string()),
    })
}

fn finish_request() -> Json<RegisterFinishRequest> {
    Json(RegisterFinishRequest {
        credential: serde_json::json!({ "anything": "the backend decides" }),
        nickname: Some("test key".to_string()),
    })
}

/// The jar a browser would send back after `/begin` set its cookies.
fn jar_from(response: &Response, name: &str) -> CookieJar {
    let value = response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| Cookie::parse(v.to_string()).ok())
        .find(|c| c.name() == name)
        .unwrap_or_else(|| panic!("no {name} cookie on the response"))
        .value()
        .to_string();
    CookieJar::new().add(Cookie::new(name.to_string(), value))
}

async fn begin_with_invite(state: &AppState, raw_token: &str) -> Response {
    register_begin(
        State(state.clone()),
        peer(),
        CookieJar::new(),
        HeaderMap::new(),
        invite_request(raw_token),
    )
    .await
}

async fn finish_with_cookie(state: &AppState, jar: CookieJar) -> Response {
    register_finish(
        State(state.clone()),
        peer(),
        jar,
        HeaderMap::new(),
        finish_request(),
    )
    .await
}

async fn user_status(superuser: &PgPool, user: Uuid) -> String {
    sqlx::query_scalar("SELECT status::text FROM auth.users WHERE id = $1")
        .bind(user)
        .fetch_one(superuser)
        .await
        .unwrap()
}

async fn credential_count(superuser: &PgPool, user: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM auth.webauthn_credentials WHERE user_id = $1")
        .bind(user)
        .fetch_one(superuser)
        .await
        .unwrap()
}

async fn invite_used(superuser: &PgPool, raw_token: &str) -> bool {
    sqlx::query_scalar("SELECT used_at IS NOT NULL FROM auth.user_invites WHERE token_hash = $1")
        .bind(hash_token(raw_token))
        .fetch_one(superuser)
        .await
        .unwrap()
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

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn register_db_invite_path_enrols_activates_signs_in_and_consumes_the_invite() {
    let _ = dotenvy::from_filename(".env.local");
    let superuser = superuser_pool();
    let raw = format!("tok-{}", Uuid::new_v4());
    let user = invited_user(&superuser, &raw).await;
    let credential_id = Uuid::new_v4().as_bytes().to_vec();
    let state = state_with(Some(credential_id.clone()));

    let begin = begin_with_invite(&state, &raw).await;
    assert_eq!(begin.status(), StatusCode::OK);
    let jar = jar_from(&begin, REGISTRATION_CEREMONY_COOKIE);

    let finish = finish_with_cookie(&state, jar).await;

    assert_eq!(finish.status(), StatusCode::OK);
    // A session cookie rides along: accepting an invite signs the user in.
    assert!(
        finish
            .headers()
            .get_all(SET_COOKIE)
            .iter()
            .any(|v| v.to_str().unwrap_or("").starts_with(SESSION_COOKIE_NAME)),
        "the invite path must issue a session cookie"
    );
    let body = body_json(finish).await;
    assert_eq!(body["success"], true);
    assert_eq!(body["session_issued"], true);

    assert_eq!(user_status(&superuser, user).await, "active");
    assert!(invite_used(&superuser, &raw).await);
    assert_eq!(credential_count(&superuser, user).await, 1);
    let stored: Vec<u8> = sqlx::query_scalar(
        "SELECT credential_id FROM auth.webauthn_credentials WHERE user_id = $1",
    )
    .bind(user)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert_eq!(stored, credential_id);
    let device_bound: bool =
        sqlx::query_scalar("SELECT device_bound FROM auth.webauthn_credentials WHERE user_id = $1")
            .bind(user)
            .fetch_one(&superuser)
            .await
            .unwrap();
    assert!(
        !device_bound,
        "device_bound comes from the credential, not the column default"
    );
    assert_eq!(audit_count(&superuser, "passkey_registered", user).await, 1);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn register_db_a_failed_verification_leaves_the_invite_usable_for_a_retry() {
    let _ = dotenvy::from_filename(".env.local");
    let superuser = superuser_pool();
    let raw = format!("tok-{}", Uuid::new_v4());
    let user = invited_user(&superuser, &raw).await;

    // First attempt: the authenticator prompt is cancelled / verification fails.
    let failing = state_with(None);
    let begin = begin_with_invite(&failing, &raw).await;
    let finish = finish_with_cookie(&failing, jar_from(&begin, REGISTRATION_CEREMONY_COOKIE)).await;
    assert_eq!(finish.status(), StatusCode::BAD_REQUEST);

    assert_eq!(user_status(&superuser, user).await, "invited");
    assert!(!invite_used(&superuser, &raw).await);
    assert_eq!(credential_count(&superuser, user).await, 0);
    assert_eq!(
        audit_count(&superuser, "registration_failed", user).await,
        1
    );

    // The retry with the same invite works.
    let working = state_with(Some(Uuid::new_v4().as_bytes().to_vec()));
    let begin = begin_with_invite(&working, &raw).await;
    assert_eq!(begin.status(), StatusCode::OK);
    let finish = finish_with_cookie(&working, jar_from(&begin, REGISTRATION_CEREMONY_COOKIE)).await;
    assert_eq!(finish.status(), StatusCode::OK);
    assert_eq!(user_status(&superuser, user).await, "active");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn register_db_an_invite_that_expires_mid_ceremony_writes_nothing() {
    let _ = dotenvy::from_filename(".env.local");
    let superuser = superuser_pool();
    let raw = format!("tok-{}", Uuid::new_v4());
    let user = invited_user(&superuser, &raw).await;
    let state = state_with(Some(Uuid::new_v4().as_bytes().to_vec()));

    let begin = begin_with_invite(&state, &raw).await;
    assert_eq!(begin.status(), StatusCode::OK);
    // The invite lapses between /begin and /finish.
    sqlx::query(
        "UPDATE auth.user_invites SET expires_at = now() - interval '1 minute' WHERE user_id = $1",
    )
    .bind(user)
    .execute(&superuser)
    .await
    .unwrap();

    let finish = finish_with_cookie(&state, jar_from(&begin, REGISTRATION_CEREMONY_COOKIE)).await;

    // The same opaque 403 as every other refusal -- and, the point of the
    // single transaction, NOTHING was written: no credential, still invited.
    assert_eq!(finish.status(), StatusCode::FORBIDDEN);
    assert_eq!(credential_count(&superuser, user).await, 0);
    assert_eq!(user_status(&superuser, user).await, "invited");
    assert_eq!(
        audit_count(&superuser, "registration_failed", user).await,
        1
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn register_db_a_credential_insert_failure_does_not_consume_the_invite() {
    let _ = dotenvy::from_filename(".env.local");
    let superuser = superuser_pool();
    let raw = format!("tok-{}", Uuid::new_v4());
    let user = invited_user(&superuser, &raw).await;

    // Another account already holds this credential id (the column is UNIQUE),
    // so the INSERT inside the enrolment transaction fails.
    let taken = Uuid::new_v4().as_bytes().to_vec();
    let other = create_user(&superuser, "holder").await;
    sqlx::query(
        "INSERT INTO auth.webauthn_credentials (user_id, credential_id, passkey_data, device_bound)
         VALUES ($1, $2, '{}'::jsonb, false)",
    )
    .bind(other)
    .bind(&taken)
    .execute(&superuser)
    .await
    .unwrap();
    let state = state_with(Some(taken));

    let begin = begin_with_invite(&state, &raw).await;
    let finish = finish_with_cookie(&state, jar_from(&begin, REGISTRATION_CEREMONY_COOKIE)).await;

    assert_eq!(finish.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(user_status(&superuser, user).await, "invited");
    assert!(!invite_used(&superuser, &raw).await);
    assert_eq!(credential_count(&superuser, user).await, 0);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn register_db_an_invite_cannot_be_used_twice() {
    let _ = dotenvy::from_filename(".env.local");
    let superuser = superuser_pool();
    let raw = format!("tok-{}", Uuid::new_v4());
    invited_user(&superuser, &raw).await;
    let state = state_with(Some(Uuid::new_v4().as_bytes().to_vec()));

    let begin = begin_with_invite(&state, &raw).await;
    let finish = finish_with_cookie(&state, jar_from(&begin, REGISTRATION_CEREMONY_COOKIE)).await;
    assert_eq!(finish.status(), StatusCode::OK);

    let second = begin_with_invite(&state, &raw).await;

    assert_eq!(second.status(), StatusCode::FORBIDDEN);
}

/// A signed-in, step-up-elevated session for `user`; returns the raw cookie
/// value.
async fn elevated_session(superuser: &PgPool, user: Uuid) -> String {
    let (raw, hash) = crate::auth::generate_token();
    sqlx::query(
        "SELECT auth.create_session($1, $2, now() + interval '1 hour', '127.0.0.1'::inet, 'test', false)",
    )
    .bind(user)
    .bind(&hash)
    .execute(superuser)
    .await
    .unwrap();
    sqlx::query("UPDATE auth.sessions SET elevated_until = now() + interval '10 minutes' WHERE token_hash = $1")
        .bind(&hash)
        .execute(superuser)
        .await
        .unwrap();
    raw
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn register_db_a_signed_in_user_adds_a_passkey_to_themselves_and_ignores_someone_elses_invite(
) {
    let _ = dotenvy::from_filename(".env.local");
    let superuser = superuser_pool();
    let me = create_user(&superuser, "self").await;
    let victim_token = format!("tok-{}", Uuid::new_v4());
    let victim = invited_user(&superuser, &victim_token).await;
    let session = elevated_session(&superuser, me).await;
    let credential_id = Uuid::new_v4().as_bytes().to_vec();
    let state = state_with(Some(credential_id));

    // Signed in AND carrying a token for another account: the token is
    // ignored outright, the target comes from the session.
    let jar = CookieJar::new().add(Cookie::new(SESSION_COOKIE_NAME, session));
    let begin = register_begin(
        State(state.clone()),
        peer(),
        jar.clone(),
        HeaderMap::new(),
        invite_request(&victim_token),
    )
    .await;
    assert_eq!(begin.status(), StatusCode::OK);
    let ceremony = jar_from(&begin, REGISTRATION_CEREMONY_COOKIE);
    let jar = jar.add(ceremony.get(REGISTRATION_CEREMONY_COOKIE).unwrap().clone());

    let finish = finish_with_cookie(&state, jar).await;

    assert_eq!(finish.status(), StatusCode::OK);
    let body = body_json(finish).await;
    assert_eq!(
        body["session_issued"], false,
        "an existing session is kept, not replaced"
    );
    assert_eq!(credential_count(&superuser, me).await, 1);
    // The invited account was not touched.
    assert_eq!(credential_count(&superuser, victim).await, 0);
    assert_eq!(user_status(&superuser, victim).await, "invited");
    assert!(!invite_used(&superuser, &victim_token).await);
}
