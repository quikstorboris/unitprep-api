//! Real-database tests for `auth.session_profile` (migration
//! `20261029160000`), the single-statement read behind `GET /health/whoami`.
//!
//! The function is SECURITY DEFINER and bypasses row-level security, so its
//! safety rests entirely on what these tests pin: it can only describe the
//! owner of a live session, it follows `resolve_session`'s validity rules
//! exactly, it never returns another user's data, and its privileges and
//! search_path are locked down. Every test is `#[ignore]`d and needs the
//! local ephemeral `test-db` (see `clickup_db_tests.rs` for the shared
//! fixtures), never Neon:
//!
//! ```text
//! TEST_DATABASE_URL=postgres://app_service:app_service@127.0.0.1:5433/unitprep_test \
//!   cargo test -- --ignored session_profile_db
//! ```

use sqlx::PgPool;
use uuid::Uuid;

use super::clickup_db_tests::{create_user, superuser_pool};
use super::session_resolution_db_tests::insert_session;
use crate::auth::{query_session_profile, SessionProfile};

async fn add_totp_credential(superuser: &PgPool, user_id: Uuid, confirmed: bool) {
    sqlx::query(
        "INSERT INTO auth.totp_credentials (user_id, secret_encrypted, confirmed_at)
         VALUES ($1, '\\x00'::bytea, CASE WHEN $2 THEN now() END)",
    )
    .bind(user_id)
    .bind(confirmed)
    .execute(superuser)
    .await
    .expect("fixture TOTP credential must insert");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn session_profile_db_returns_the_callers_own_name_and_totp_state() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();
    let superuser = superuser_pool();

    let user_id = create_user(&superuser, "profile").await;
    let token_hash = insert_session(&superuser, user_id, 0).await;

    let profile = query_session_profile(&token_hash, &app)
        .await
        .expect("session_profile must be valid SQL against the real schema");
    assert_eq!(
        profile,
        Some(SessionProfile {
            first_name: "Test".to_string(),
            last_name: "profile".to_string(),
            totp_enrolled: false,
        }),
        "a user with no TOTP credential is not enrolled"
    );

    add_totp_credential(&superuser, user_id, false).await;
    assert!(
        !query_session_profile(&token_hash, &app)
            .await
            .unwrap()
            .unwrap()
            .totp_enrolled,
        "an unconfirmed TOTP credential must not count as enrolled"
    );

    sqlx::query("UPDATE auth.totp_credentials SET confirmed_at = now() WHERE user_id = $1")
        .bind(user_id)
        .execute(&superuser)
        .await
        .unwrap();
    assert!(
        query_session_profile(&token_hash, &app)
            .await
            .unwrap()
            .unwrap()
            .totp_enrolled,
        "a confirmed TOTP credential must count as enrolled"
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn session_profile_db_never_returns_another_users_profile() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();
    let superuser = superuser_pool();

    let alice = create_user(&superuser, "alice").await;
    let bob = create_user(&superuser, "bob").await;
    add_totp_credential(&superuser, bob, true).await;
    let alice_token = insert_session(&superuser, alice, 0).await;
    let bob_token = insert_session(&superuser, bob, 0).await;

    let alice_profile = query_session_profile(&alice_token, &app)
        .await
        .unwrap()
        .unwrap();
    let bob_profile = query_session_profile(&bob_token, &app)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(alice_profile.last_name, "alice");
    assert!(
        !alice_profile.totp_enrolled,
        "alice must not inherit bob's TOTP state"
    );
    assert_eq!(bob_profile.last_name, "bob");
    assert!(bob_profile.totp_enrolled);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn session_profile_db_follows_resolve_sessions_validity_rules() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();
    let superuser = superuser_pool();

    // A token that matches no session at all.
    let (_, unknown) = crate::auth::generate_token();
    assert_eq!(query_session_profile(&unknown, &app).await.unwrap(), None);

    // Idle past the window. Called with an explicit 30-minute window, not
    // through `query_session_profile`, whose window comes from the
    // SESSION_IDLE_TIMEOUT_MINUTES environment variable (which a dev
    // `.env.local` may set far above 30).
    let idle_user = create_user(&superuser, "idle").await;
    let idle = insert_session(&superuser, idle_user, 31).await;
    let idle_rows: Vec<(String,)> =
        sqlx::query_as("SELECT last_name FROM auth.session_profile($1, 30)")
            .bind(&idle)
            .fetch_all(&app)
            .await
            .unwrap();
    assert!(
        idle_rows.is_empty(),
        "an idle-expired session must yield no profile"
    );
    let fresh = insert_session(&superuser, idle_user, 29).await;
    let fresh_rows: Vec<(String,)> =
        sqlx::query_as("SELECT last_name FROM auth.session_profile($1, 30)")
            .bind(&fresh)
            .fetch_all(&app)
            .await
            .unwrap();
    assert_eq!(
        fresh_rows.len(),
        1,
        "a session inside the idle window must yield its profile"
    );

    // Past its absolute expiry.
    let expired_user = create_user(&superuser, "expired").await;
    let expired = insert_session(&superuser, expired_user, 0).await;
    sqlx::query(
        "UPDATE auth.sessions SET expires_at = now() - interval '1 minute' WHERE token_hash = $1",
    )
    .bind(&expired)
    .execute(&superuser)
    .await
    .unwrap();
    assert_eq!(query_session_profile(&expired, &app).await.unwrap(), None);

    // Revoked: effective on the very next call.
    let revoked_user = create_user(&superuser, "revoked").await;
    let revoked = insert_session(&superuser, revoked_user, 0).await;
    assert!(query_session_profile(&revoked, &app)
        .await
        .unwrap()
        .is_some());
    sqlx::query("UPDATE auth.sessions SET revoked_at = now() WHERE token_hash = $1")
        .bind(&revoked)
        .execute(&superuser)
        .await
        .unwrap();
    assert_eq!(query_session_profile(&revoked, &app).await.unwrap(), None);

    // Deactivated user.
    let deactivated_user = create_user(&superuser, "deactivated").await;
    let deactivated = insert_session(&superuser, deactivated_user, 0).await;
    assert!(query_session_profile(&deactivated, &app)
        .await
        .unwrap()
        .is_some());
    sqlx::query("UPDATE auth.users SET status = 'deactivated' WHERE id = $1")
        .bind(deactivated_user)
        .execute(&superuser)
        .await
        .unwrap();
    assert_eq!(
        query_session_profile(&deactivated, &app).await.unwrap(),
        None
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn session_profile_db_privileges_and_search_path_are_locked_down() {
    let _ = dotenvy::from_filename(".env.local");
    let superuser = superuser_pool();

    let (definer, search_path_pinned, public_can_execute, app_can_execute): (
        bool,
        bool,
        bool,
        bool,
    ) = sqlx::query_as(
        "SELECT p.prosecdef,
                    COALESCE(p.proconfig::text LIKE '%search_path=%', false),
                    EXISTS (SELECT 1
                              FROM aclexplode(COALESCE(p.proacl, acldefault('f', p.proowner))) a
                             WHERE a.grantee = 0 AND a.privilege_type = 'EXECUTE'),
                    has_function_privilege('app_service', p.oid, 'EXECUTE')
               FROM pg_proc p
              WHERE p.oid = 'auth.session_profile(bytea, integer)'::regprocedure",
    )
    .fetch_one(&superuser)
    .await
    .expect("auth.session_profile must exist");

    assert!(definer, "session_profile is SECURITY DEFINER by design");
    assert!(
        search_path_pinned,
        "a SECURITY DEFINER function must pin its search_path"
    );
    assert!(!public_can_execute, "EXECUTE must be revoked from PUBLIC");
    assert!(app_can_execute, "app_service must be able to execute it");
}
