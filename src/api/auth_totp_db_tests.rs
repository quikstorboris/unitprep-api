//! Real-database tests for TOTP enrolment and step-up (`auth_totp`): the
//! gate in front of sensitive actions. The offline tests only cover the
//! "not configured" branches; everything that matters -- the lockout,
//! the replay guard, the re-enrolment gate, the audit trail -- lives in SQL
//! plus the handler's use of it, so it is proven here against a real
//! Postgres (see `clickup_db_tests`' module doc for the setup).
//!
//! Codes are computed the way an authenticator app would: from the base32
//! secret `enroll_begin` hands back, or from a secret the fixture stored
//! itself. Every test shares the process-wide `TOTP_ENCRYPTION_KEY`, so
//! they run under the same `serial(totp_env)` key as the offline tests.

use axum::extract::{Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::PgPool;
use totp_rs::{Algorithm, Secret, TOTP};
use uuid::Uuid;

use super::auth_totp::{enroll_begin, enroll_confirm, step_up, TotpCodeRequest};
use super::clickup_db_tests::{body_json, caller, create_user, superuser_pool};
use crate::api::test_support::empty_state;
use crate::api::AppState;
use crate::auth::{audit_log, encrypt_secret, generate_secret, generate_token, AuthenticatedUser};

fn ensure_key() {
    std::env::set_var(
        "TOTP_ENCRYPTION_KEY",
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
    );
}

fn state() -> AppState {
    let _ = dotenvy::from_filename(".env.local");
    ensure_key();
    AppState {
        db: crate::db::connect_test(),
        ..empty_state()
    }
}

fn totp_for(secret: Vec<u8>) -> TOTP {
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        secret,
        Some("UnitPrep".to_string()),
        "fixture".to_string(),
    )
    .expect("fixture TOTP must build")
}

/// The code an authenticator would show right now for a base32 secret.
fn current_code(secret_base32: &str) -> String {
    let bytes = Secret::Encoded(secret_base32.to_string())
        .to_bytes()
        .expect("secret must decode");
    totp_for(bytes).generate_current().unwrap()
}

/// A six-digit code that is wrong for the previous, current and next step.
fn wrong_code_for(secret: &[u8]) -> String {
    let totp = totp_for(secret.to_vec());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let valid: Vec<String> = (-1i64..=1)
        .map(|offset| totp.generate((now as i64 + offset * 30) as u64))
        .collect();
    (0..1000)
        .map(|n| format!("{n:06}"))
        .find(|candidate| !valid.contains(candidate))
        .unwrap()
}

/// A signed-in caller with a real session row, so `auth.record_step_up`
/// has something to elevate.
async fn signed_in(superuser: &PgPool, label: &str) -> AuthenticatedUser {
    ensure_key();
    let user_id = create_user(superuser, label).await;
    let (_, token_hash) = generate_token();
    sqlx::query(
        "INSERT INTO auth.sessions (user_id, token_hash, expires_at)
         VALUES ($1, $2, now() + interval '1 hour')",
    )
    .bind(user_id)
    .bind(&token_hash)
    .execute(superuser)
    .await
    .unwrap();
    let mut user = caller(user_id, &["onboarding_manager"], &[]);
    user.token_hash = token_hash;
    user
}

/// Stores an already-CONFIRMED credential and returns its raw secret.
async fn confirmed_credential(superuser: &PgPool, user_id: Uuid) -> Vec<u8> {
    let secret = generate_secret().to_vec();
    let blob = encrypt_secret(user_id, &secret).unwrap();
    sqlx::query(
        "INSERT INTO auth.totp_credentials (user_id, secret_encrypted, confirmed_at)
         VALUES ($1, $2, now())",
    )
    .bind(user_id)
    .bind(blob)
    .execute(superuser)
    .await
    .expect("fixture credential must insert");
    secret
}

///  is deliberately not ; tests that call several
/// handlers as the same caller rebuild it field by field.
fn dup(user: &AuthenticatedUser) -> AuthenticatedUser {
    AuthenticatedUser {
        user_id: user.user_id,
        role_keys: user.role_keys.clone(),
        permission_keys: user.permission_keys.clone(),
        token_hash: user.token_hash.clone(),
        elevated_until: user.elevated_until,
        requires_step_up: user.requires_step_up,
        passkey_reverified_until: user.passkey_reverified_until,
    }
}

fn code_request(code: &str) -> Json<TotpCodeRequest> {
    Json(TotpCodeRequest {
        code: code.to_string(),
    })
}

async fn step_up_with(user: &AuthenticatedUser, code: &str) -> Response {
    step_up(
        State(state()),
        dup(user),
        HeaderMap::new(),
        code_request(code),
    )
    .await
}

async fn error_code(response: Response) -> String {
    let body: Value = body_json(response).await;
    body["error"].as_str().unwrap_or_default().to_string()
}

async fn failed_attempts(superuser: &PgPool, user_id: Uuid) -> i32 {
    sqlx::query_scalar("SELECT failed_attempts FROM auth.totp_credentials WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(superuser)
        .await
        .unwrap()
}

async fn audit_count(superuser: &PgPool, event: &str, user_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM auth.auth_audit_logs WHERE event_type = $1 AND actor_user_id = $2",
    )
    .bind(event)
    .bind(user_id)
    .fetch_one(superuser)
    .await
    .unwrap()
}

async fn elevated_until(
    superuser: &PgPool,
    token_hash: &[u8],
) -> Option<chrono::DateTime<chrono::Utc>> {
    sqlx::query_scalar("SELECT elevated_until FROM auth.sessions WHERE token_hash = $1")
        .bind(token_hash)
        .fetch_one(superuser)
        .await
        .unwrap()
}

// ------------------------------------------------------------ enrolment

#[tokio::test]
#[serial_test::serial(totp_env)]
#[ignore = "needs the local test-db"]
async fn totp_db_enrolment_confirms_with_a_correct_code_and_audits_without_the_secret() {
    let superuser = superuser_pool();
    let user = signed_in(&superuser, "enroller").await;

    let begin = enroll_begin(State(state()), dup(&user), HeaderMap::new()).await;
    assert_eq!(begin.status(), StatusCode::OK);
    let secret = body_json(begin).await["secret"]
        .as_str()
        .expect("secret in the response")
        .to_string();

    // Pending, not yet usable as a step-up factor.
    let pending = step_up_with(&user, &current_code(&secret)).await;
    assert_eq!(pending.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(pending).await, "totp_not_enrolled");

    let confirm = enroll_confirm(
        State(state()),
        dup(&user),
        HeaderMap::new(),
        code_request(&current_code(&secret)),
    )
    .await;
    assert_eq!(confirm.status(), StatusCode::OK);

    let confirmed: bool = sqlx::query_scalar(
        "SELECT confirmed_at IS NOT NULL FROM auth.totp_credentials WHERE user_id = $1",
    )
    .bind(user.user_id)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert!(confirmed);
    assert_eq!(
        audit_count(
            &superuser,
            audit_log::event::TOTP_ENROLMENT_STARTED,
            user.user_id
        )
        .await,
        1
    );
    assert_eq!(
        audit_count(&superuser, audit_log::event::TOTP_ENROLLED, user.user_id).await,
        1
    );

    // An audit trail is not a place to keep a shared secret.
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM auth.auth_audit_logs
          WHERE actor_user_id = $1
            AND (metadata::text ILIKE '%' || $2 || '%'
              OR coalesce(before_state::text, '') ILIKE '%' || $2 || '%'
              OR coalesce(after_state::text, '') ILIKE '%' || $2 || '%')",
    )
    .bind(user.user_id)
    .bind(&secret)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert_eq!(
        leaked, 0,
        "the shared secret must never reach the audit log"
    );
}

#[tokio::test]
#[serial_test::serial(totp_env)]
#[ignore = "needs the local test-db"]
async fn totp_db_a_wrong_enrolment_code_is_rejected_counted_and_audited() {
    let superuser = superuser_pool();
    let user = signed_in(&superuser, "fumbler").await;
    let begin = enroll_begin(State(state()), dup(&user), HeaderMap::new()).await;
    let secret_b32 = body_json(begin).await["secret"]
        .as_str()
        .unwrap()
        .to_string();
    let secret = Secret::Encoded(secret_b32).to_bytes().unwrap();

    let response = enroll_confirm(
        State(state()),
        dup(&user),
        HeaderMap::new(),
        code_request(&wrong_code_for(&secret)),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(response).await, "totp_code_rejected");
    assert_eq!(failed_attempts(&superuser, user.user_id).await, 1);
    assert_eq!(
        audit_count(
            &superuser,
            audit_log::event::TOTP_ENROLMENT_FAILED,
            user.user_id
        )
        .await,
        1
    );
}

#[tokio::test]
#[serial_test::serial(totp_env)]
#[ignore = "needs the local test-db"]
async fn totp_db_enrolment_confirmation_locks_out_after_five_wrong_codes_even_for_the_right_one() {
    let superuser = superuser_pool();
    let user = signed_in(&superuser, "guesser").await;
    let begin = enroll_begin(State(state()), dup(&user), HeaderMap::new()).await;
    let secret_b32 = body_json(begin).await["secret"]
        .as_str()
        .unwrap()
        .to_string();
    let secret = Secret::Encoded(secret_b32.clone()).to_bytes().unwrap();

    for _ in 0..5 {
        let response = enroll_confirm(
            State(state()),
            dup(&user),
            HeaderMap::new(),
            code_request(&wrong_code_for(&secret)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    let locked = enroll_confirm(
        State(state()),
        dup(&user),
        HeaderMap::new(),
        code_request(&current_code(&secret_b32)),
    )
    .await;
    assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error_code(locked).await, "totp_locked_out");
}

#[tokio::test]
#[serial_test::serial(totp_env)]
#[ignore = "needs the local test-db"]
async fn totp_db_confirming_with_no_enrolment_in_progress_reads_as_a_wrong_code() {
    let superuser = superuser_pool();
    let user = signed_in(&superuser, "nobody").await;

    let response = enroll_confirm(
        State(state()),
        user,
        HeaderMap::new(),
        code_request("123456"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(response).await, "totp_code_rejected");
}

#[tokio::test]
#[serial_test::serial(totp_env)]
#[ignore = "needs the local test-db"]
async fn totp_db_replacing_a_confirmed_authenticator_requires_a_fresh_passkey_check() {
    let superuser = superuser_pool();
    let mut user = signed_in(&superuser, "replacer").await;
    confirmed_credential(&superuser, user.user_id).await;

    let blocked = enroll_begin(State(state()), dup(&user), HeaderMap::new()).await;
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_code(blocked).await, "passkey_reverification_required");

    user.passkey_reverified_until = Some(Utc::now() + Duration::minutes(5));
    let allowed = enroll_begin(State(state()), dup(&user), HeaderMap::new()).await;
    assert_eq!(allowed.status(), StatusCode::OK);

    // The existing factor keeps working for the whole re-enrolment window.
    let still_there: bool = sqlx::query_scalar(
        "SELECT confirmed_at IS NOT NULL FROM auth.totp_credentials WHERE user_id = $1",
    )
    .bind(user.user_id)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert!(still_there);
}

#[tokio::test]
#[serial_test::serial(totp_env)]
#[ignore = "needs the local test-db"]
async fn totp_db_an_expired_passkey_reverification_does_not_unlock_replacement() {
    let superuser = superuser_pool();
    let mut user = signed_in(&superuser, "stale-reverify").await;
    confirmed_credential(&superuser, user.user_id).await;
    user.passkey_reverified_until = Some(Utc::now() - Duration::minutes(1));

    let response = enroll_begin(State(state()), user, HeaderMap::new()).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// -------------------------------------------------------------- step-up

#[tokio::test]
#[serial_test::serial(totp_env)]
#[ignore = "needs the local test-db"]
async fn totp_db_step_up_with_a_correct_code_elevates_the_session_for_about_five_minutes() {
    let superuser = superuser_pool();
    let user = signed_in(&superuser, "stepper").await;
    let secret = confirmed_credential(&superuser, user.user_id).await;
    assert!(elevated_until(&superuser, &user.token_hash).await.is_none());

    let code = totp_for(secret).generate_current().unwrap();
    let response = step_up_with(&user, &code).await;

    assert_eq!(response.status(), StatusCode::OK);
    let until = elevated_until(&superuser, &user.token_hash)
        .await
        .expect("session must be elevated");
    let minutes = (until - Utc::now()).num_seconds() as f64 / 60.0;
    assert!(
        (4.0..=5.1).contains(&minutes),
        "elevation should last ~5 minutes, got {minutes:.2}"
    );
    assert_eq!(
        audit_count(
            &superuser,
            audit_log::event::TOTP_STEP_UP_SUCCEEDED,
            user.user_id
        )
        .await,
        1
    );
}

#[tokio::test]
#[serial_test::serial(totp_env)]
#[ignore = "needs the local test-db"]
async fn totp_db_a_code_cannot_be_replayed_after_a_successful_step_up() {
    let superuser = superuser_pool();
    let user = signed_in(&superuser, "replayed").await;
    let secret = confirmed_credential(&superuser, user.user_id).await;
    let code = totp_for(secret).generate_current().unwrap();
    assert_eq!(step_up_with(&user, &code).await.status(), StatusCode::OK);

    // Same code, same window: a shoulder-surfed code must be a dead end,
    // indistinguishable from a wrong one.
    let replay = step_up_with(&user, &code).await;

    assert_eq!(replay.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(replay).await, "totp_code_rejected");
}

#[tokio::test]
#[serial_test::serial(totp_env)]
#[ignore = "needs the local test-db"]
async fn totp_db_step_up_without_an_authenticator_says_so_and_does_not_elevate() {
    let superuser = superuser_pool();
    let user = signed_in(&superuser, "unenrolled").await;

    let response = step_up_with(&user, "123456").await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(response).await, "totp_not_enrolled");
    assert!(elevated_until(&superuser, &user.token_hash).await.is_none());
}

#[tokio::test]
#[serial_test::serial(totp_env)]
#[ignore = "needs the local test-db"]
async fn totp_db_wrong_step_up_codes_never_elevate_and_lock_the_factor_after_five() {
    let superuser = superuser_pool();
    let user = signed_in(&superuser, "bruteforcer").await;
    let secret = confirmed_credential(&superuser, user.user_id).await;

    for attempt in 1..=5 {
        let response = step_up_with(&user, &wrong_code_for(&secret)).await;
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "attempt {attempt}"
        );
    }
    assert_eq!(failed_attempts(&superuser, user.user_id).await, 5);

    // Locked: even the CORRECT code is refused...
    let correct = totp_for(secret.clone()).generate_current().unwrap();
    let locked = step_up_with(&user, &correct).await;
    assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error_code(locked).await, "totp_locked_out");
    assert!(elevated_until(&superuser, &user.token_hash).await.is_none());

    // ...and further guesses while locked are not counted, so a sustained
    // attack cannot stretch a 15-minute lock indefinitely.
    let _ = step_up_with(&user, &wrong_code_for(&secret)).await;
    assert_eq!(failed_attempts(&superuser, user.user_id).await, 5);

    // The lock is time-boxed: once it lapses the right code works again.
    sqlx::query(
        "UPDATE auth.totp_credentials SET locked_until = now() - interval '1 second' WHERE user_id = $1",
    )
    .bind(user.user_id)
    .execute(&superuser)
    .await
    .unwrap();
    let after_lock = step_up_with(&user, &correct).await;
    assert_eq!(after_lock.status(), StatusCode::OK);
    assert_eq!(
        failed_attempts(&superuser, user.user_id).await,
        0,
        "a success clears the failure counter"
    );
}

#[tokio::test]
#[serial_test::serial(totp_env)]
#[ignore = "needs the local test-db"]
async fn totp_db_a_wrong_step_up_code_is_audited_with_the_attempt_count() {
    let superuser = superuser_pool();
    let user = signed_in(&superuser, "audited").await;
    let secret = confirmed_credential(&superuser, user.user_id).await;

    let response = step_up_with(&user, &wrong_code_for(&secret)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let attempts: Value = sqlx::query_scalar(
        "SELECT metadata -> 'failed_attempts' FROM auth.auth_audit_logs
          WHERE event_type = $1 AND actor_user_id = $2",
    )
    .bind(audit_log::event::TOTP_STEP_UP_FAILED)
    .bind(user.user_id)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert_eq!(attempts, serde_json::json!(1));
}

#[tokio::test]
#[serial_test::serial(totp_env)]
#[ignore = "needs the local test-db"]
async fn totp_db_a_correct_code_for_a_session_that_just_vanished_is_unauthorized_not_confirmed() {
    let superuser = superuser_pool();
    let mut user = signed_in(&superuser, "ghost-session").await;
    let secret = confirmed_credential(&superuser, user.user_id).await;
    // The caller still holds a token, but no live session row matches it.
    user.token_hash = generate_token().1;
    let code = totp_for(secret).generate_current().unwrap();

    let response = step_up_with(&user, &code).await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
#[serial_test::serial(totp_env)]
#[ignore = "needs the local test-db"]
async fn totp_db_one_users_credential_is_never_used_to_step_up_another() {
    let superuser = superuser_pool();
    let owner = signed_in(&superuser, "owner").await;
    let other = signed_in(&superuser, "other").await;
    let secret = confirmed_credential(&superuser, owner.user_id).await;
    let code = totp_for(secret).generate_current().unwrap();

    // `other` has no authenticator of their own, so owner's code is useless
    // to them (and the secret is bound to the owner's user id on decrypt).
    let response = step_up_with(&other, &code).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(response).await, "totp_not_enrolled");
    assert!(elevated_until(&superuser, &other.token_hash)
        .await
        .is_none());
}
