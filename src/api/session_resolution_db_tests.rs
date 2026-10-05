//! Real-database tests for `auth.resolve_session`'s throttled
//! `last_seen_at` bump (migration `20261005100000`) and for
//! `begin_rls_transaction` setting both identity GUCs in one statement.
//!
//! The fast offline suite cannot prove either: both live in SQL. Every
//! test is `#[ignore]`d and needs the local ephemeral `test-db` (see
//! `clickup_db_tests.rs` for the shared fixture helpers and the run
//! command), never Neon:
//!
//! ```text
//! TEST_DATABASE_URL=postgres://app_service:app_service@127.0.0.1:5433/unitprep_test \
//!   cargo test -- --ignored session_resolution_db
//! ```
//!
//! `resolve_session` is called as `app_service` (what the server uses);
//! fixtures and read-backs go through the superuser pool, since
//! `app_service` has no UPDATE grant on `auth.sessions` -- by design.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::clickup_db_tests::{create_user, superuser_pool};

const IDLE_MINUTES: i32 = 30;

async fn insert_session(superuser: &PgPool, user_id: Uuid, last_seen_minutes_ago: i32) -> Vec<u8> {
    let (_, token_hash) = crate::auth::generate_token();
    sqlx::query(
        "INSERT INTO auth.sessions (user_id, token_hash, expires_at, last_seen_at)
         VALUES ($1, $2, now() + interval '1 hour',
                 now() - make_interval(mins => $3))",
    )
    .bind(user_id)
    .bind(&token_hash)
    .bind(last_seen_minutes_ago)
    .execute(superuser)
    .await
    .expect("fixture session must insert");
    token_hash
}

async fn last_seen(superuser: &PgPool, token_hash: &[u8]) -> DateTime<Utc> {
    sqlx::query_scalar("SELECT last_seen_at FROM auth.sessions WHERE token_hash = $1")
        .bind(token_hash)
        .fetch_one(superuser)
        .await
        .expect("fixture session must exist")
}

async fn resolve(app: &PgPool, token_hash: &[u8]) -> Option<Uuid> {
    sqlx::query_scalar("SELECT user_id FROM auth.resolve_session($1, $2)")
        .bind(token_hash)
        .bind(IDLE_MINUTES)
        .fetch_optional(app)
        .await
        .expect("resolve_session must be valid SQL against the real schema")
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn session_resolution_db_a_stale_last_seen_is_bumped_once_then_left_alone() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();
    let superuser = superuser_pool();

    let user_id = create_user(&superuser, "throttle").await;
    // 5 minutes old: well past the 60 s throttle, well inside the idle window.
    let token_hash = insert_session(&superuser, user_id, 5).await;
    let before = last_seen(&superuser, &token_hash).await;

    assert_eq!(resolve(&app, &token_hash).await, Some(user_id));
    let after_first = last_seen(&superuser, &token_hash).await;
    assert!(
        after_first > before,
        "a last_seen_at older than the throttle interval must be bumped"
    );

    // Immediately again: inside the throttle interval, so no write.
    assert_eq!(
        resolve(&app, &token_hash).await,
        Some(user_id),
        "the throttled call must still return the session"
    );
    assert_eq!(
        last_seen(&superuser, &token_hash).await,
        after_first,
        "a second resolve inside the throttle interval must not rewrite last_seen_at"
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn session_resolution_db_an_idle_session_is_rejected_and_never_resurrected() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();
    let superuser = superuser_pool();

    let user_id = create_user(&superuser, "idle").await;
    let token_hash = insert_session(&superuser, user_id, IDLE_MINUTES + 1).await;
    let before = last_seen(&superuser, &token_hash).await;

    assert_eq!(resolve(&app, &token_hash).await, None);
    assert_eq!(resolve(&app, &token_hash).await, None);
    assert_eq!(
        last_seen(&superuser, &token_hash).await,
        before,
        "an idle-expired session must not have its last_seen_at advanced by a later request"
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn session_resolution_db_revoked_and_deactivated_sessions_are_rejected() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();
    let superuser = superuser_pool();

    let revoked_user = create_user(&superuser, "revoked").await;
    let revoked = insert_session(&superuser, revoked_user, 0).await;
    assert_eq!(resolve(&app, &revoked).await, Some(revoked_user));
    sqlx::query("UPDATE auth.sessions SET revoked_at = now() WHERE token_hash = $1")
        .bind(&revoked)
        .execute(&superuser)
        .await
        .unwrap();
    assert_eq!(
        resolve(&app, &revoked).await,
        None,
        "revocation must take effect on the very next request, throttle or not"
    );

    let deactivated_user = create_user(&superuser, "deactivated").await;
    let deactivated = insert_session(&superuser, deactivated_user, 0).await;
    assert_eq!(resolve(&app, &deactivated).await, Some(deactivated_user));
    sqlx::query("UPDATE auth.users SET status = 'deactivated' WHERE id = $1")
        .bind(deactivated_user)
        .execute(&superuser)
        .await
        .unwrap();
    assert_eq!(
        resolve(&app, &deactivated).await,
        None,
        "deactivation must take effect on the very next request, throttle or not"
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn session_resolution_db_returns_the_same_columns_as_before_the_throttle() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();
    let superuser = superuser_pool();

    let user_id = create_user(&superuser, "columns").await;
    let token_hash = insert_session(&superuser, user_id, 5).await;
    sqlx::query(
        "UPDATE auth.sessions
            SET requires_step_up = true,
                elevated_until = now() + interval '5 minutes'
          WHERE token_hash = $1",
    )
    .bind(&token_hash)
    .execute(&superuser)
    .await
    .unwrap();

    // The full row, selecting the same six columns, in the same order, as
    // the extractor's `query_session`.
    #[allow(clippy::type_complexity)]
    let session: (
        Uuid,
        Option<Vec<String>>,
        Option<Vec<String>>,
        Option<DateTime<Utc>>,
        bool,
        Option<DateTime<Utc>>,
    ) = sqlx::query_as(
        "SELECT user_id, role_keys, permission_keys, elevated_until, requires_step_up, \
         passkey_reverified_until FROM auth.resolve_session($1, $2)",
    )
    .bind(&token_hash)
    .bind(IDLE_MINUTES)
    .fetch_one(&app)
    .await
    .expect("the fixture session must resolve through the real schema");

    assert_eq!(session.0, user_id);
    assert!(session.4, "requires_step_up must come back as set");
    assert!(session.3.is_some(), "elevated_until must come back as set");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn session_resolution_db_begin_rls_transaction_sets_both_identity_gucs() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();

    let user_id = Uuid::new_v4();
    let roles = vec!["admin".to_string(), "onboarding_manager".to_string()];

    let mut tx = crate::auth::begin_rls_transaction(&app, user_id, &roles)
        .await
        .expect("transaction must begin");
    let (id, role_csv): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT current_setting('app.current_user_id', true),
                current_setting('app.current_user_roles', true)",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    tx.rollback().await.unwrap();

    assert_eq!(id.as_deref(), Some(user_id.to_string().as_str()));
    assert_eq!(role_csv.as_deref(), Some("admin,onboarding_manager"));

    // And they must not leak onto the pooled connection afterwards.
    let leaked: Option<String> =
        sqlx::query_scalar("SELECT current_setting('app.current_user_id', true)")
            .fetch_one(&app)
            .await
            .unwrap();
    assert!(
        leaked.as_deref().unwrap_or("").is_empty(),
        "is_local set_config must reset when the transaction ends"
    );
}
