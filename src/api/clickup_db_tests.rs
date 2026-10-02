//! Real-database tests for the per-user permission grants and the ClickUp
//! connection -- the parts the fast offline suite structurally cannot
//! prove, because they live in SQL (the `resolve_session` union, the
//! `directly_grantable` trigger, the RLS policies) or in the handlers'
//! interaction with those.
//!
//! Every test is `#[ignore]`d: they need the local ephemeral `test-db`
//! (see docker-compose.yml / `scripts/bootstrap_test_db.sh`), never Neon.
//! Run with:
//!
//! ```text
//! TEST_DATABASE_URL=postgres://app_service:app_service@127.0.0.1:5433/unitprep_test \
//!   cargo test -- --ignored clickup_db
//! ```
//!
//! Two pools are used on purpose. Fixtures (users, sessions) are written
//! as the local superuser, since `app_service` cannot and should not be
//! able to create accounts. Everything *under test* runs as
//! `app_service` through `begin_rls_transaction`, so RLS genuinely
//! applies -- connecting as the table owner would bypass every policy
//! and make these tests meaningless.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Json, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use uuid::Uuid;

use crate::api::test_support::{empty_state, FakeEnvSource};
use crate::api::{auth_user_permissions, clickup_connection, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};

const ENCRYPTION_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Local-only superuser connection for writing fixtures, derived from
/// the same host/port the app-role URL uses. Refuses anything that looks
/// like Neon, same backstop as `db::connect_test`.
pub(super) fn superuser_pool() -> PgPool {
    let app_url = std::env::var("TEST_DATABASE_URL")
        .expect("TEST_DATABASE_URL must point at the local test-db");
    assert!(
        !app_url.contains("neon.tech"),
        "refusing to run fixtures against Neon"
    );

    let host_part = app_url
        .split('@')
        .nth(1)
        .expect("TEST_DATABASE_URL must contain credentials and a host");

    PgPoolOptions::new()
        .max_connections(2)
        .connect_lazy(&format!("postgres://postgres:postgres@{host_part}"))
        .expect("superuser URL must be well-formed")
}

pub(super) async fn create_user(superuser: &PgPool, label: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO auth.users (id, email, first_name, last_name, company, status)
         VALUES ($1, $2, 'Test', $3, 'quikstor', 'active')",
    )
    .bind(id)
    .bind(format!("{label}-{id}@example.test"))
    .bind(label)
    .execute(superuser)
    .await
    .expect("fixture user must insert");
    id
}

pub(super) fn caller(user_id: Uuid, roles: &[&str], permissions: &[&str]) -> AuthenticatedUser {
    AuthenticatedUser {
        user_id,
        role_keys: roles.iter().map(|r| r.to_string()).collect(),
        permission_keys: permissions.iter().map(|p| p.to_string()).collect(),
        token_hash: vec![0u8; 32],
        elevated_until: None,
        requires_step_up: false,
        passkey_reverified_until: None,
    }
}

fn local_addr() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
}

pub(super) async fn body_json(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body must read");
    serde_json::from_slice(&bytes).expect("response body must be JSON")
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn clickup_db_a_direct_grant_is_unioned_into_the_resolved_session() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();
    let superuser = superuser_pool();

    let user_id = create_user(&superuser, "resolve").await;
    let (_, token_hash) = crate::auth::generate_token();

    sqlx::query(
        "INSERT INTO auth.sessions (user_id, token_hash, expires_at)
         VALUES ($1, $2, now() + interval '1 hour')",
    )
    .bind(user_id)
    .bind(&token_hash)
    .execute(&superuser)
    .await
    .unwrap();

    // Before any grant: no roles, no direct permissions.
    let before: (Option<Vec<String>>,) =
        sqlx::query_as("SELECT permission_keys FROM auth.resolve_session($1, 60)")
            .bind(&token_hash)
            .fetch_one(&app)
            .await
            .unwrap();
    assert!(before.0.is_none() || before.0.unwrap().is_empty());

    sqlx::query(
        "INSERT INTO auth.user_permissions (user_id, permission_key) VALUES ($1, 'integrations.clickup')",
    )
    .bind(user_id)
    .execute(&superuser)
    .await
    .unwrap();

    let after: (Option<Vec<String>>,) =
        sqlx::query_as("SELECT permission_keys FROM auth.resolve_session($1, 60)")
            .bind(&token_hash)
            .fetch_one(&app)
            .await
            .unwrap();
    assert_eq!(after.0, Some(vec!["integrations.clickup".to_string()]));
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn clickup_db_a_permission_that_is_not_directly_grantable_is_refused_by_the_database() {
    let superuser = superuser_pool();
    let user_id = create_user(&superuser, "ungrantable").await;

    // users.manage_roles is a real permission, but role-only.
    let result = sqlx::query(
        "INSERT INTO auth.user_permissions (user_id, permission_key) VALUES ($1, 'users.manage_roles')",
    )
    .bind(user_id)
    .execute(&superuser)
    .await;

    let err = result.expect_err("the trigger must refuse a role-only permission");
    assert!(
        err.to_string().contains("cannot be granted directly"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn clickup_db_rls_lets_only_granter_roles_write_grants_and_never_on_oneself() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();
    let superuser = superuser_pool();

    let actor = create_user(&superuser, "actor").await;
    let target = create_user(&superuser, "target").await;

    let insert = "INSERT INTO auth.user_permissions (user_id, permission_key, granted_by)
                  VALUES ($1, 'integrations.clickup', $2)";

    // A user with no granter role is refused.
    let mut tx = begin_rls_transaction(&app, actor, &["onboarding_manager".to_string()])
        .await
        .unwrap();
    let denied = sqlx::query(insert)
        .bind(target)
        .bind(actor)
        .execute(&mut *tx)
        .await;
    assert!(
        denied.is_err(),
        "a non-granter role must not be able to grant"
    );
    drop(tx);

    // An admin may grant to someone else...
    let mut tx = begin_rls_transaction(&app, actor, &["admin".to_string()])
        .await
        .unwrap();
    sqlx::query(insert)
        .bind(target)
        .bind(actor)
        .execute(&mut *tx)
        .await
        .expect("an admin may grant to another user");
    tx.commit().await.unwrap();

    // ...but never to themselves.
    let mut tx = begin_rls_transaction(&app, actor, &["admin".to_string()])
        .await
        .unwrap();
    let own = sqlx::query(insert)
        .bind(actor)
        .bind(actor)
        .execute(&mut *tx)
        .await;
    assert!(
        own.is_err(),
        "an admin must not be able to grant to themselves"
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn clickup_db_credentials_rls_isolates_each_users_row_even_from_admins() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();
    let superuser = superuser_pool();

    let owner = create_user(&superuser, "owner").await;
    let other = create_user(&superuser, "other").await;

    let mut tx = begin_rls_transaction(&app, owner, &[]).await.unwrap();
    sqlx::query(
        "INSERT INTO integrations.user_clickup_credentials (user_id, token_ciphertext)
         VALUES ($1, '\\x00'::bytea)",
    )
    .bind(owner)
    .execute(&mut *tx)
    .await
    .expect("a user may store their own credential");
    tx.commit().await.unwrap();

    // Another user, and even an admin, sees nothing and deletes nothing.
    for (viewer, roles) in [(other, vec![]), (other, vec!["admin".to_string()])] {
        let mut tx = begin_rls_transaction(&app, viewer, &roles).await.unwrap();
        let seen: i64 =
            sqlx::query_scalar("SELECT count(*) FROM integrations.user_clickup_credentials")
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(seen, 0, "no one else may see another user's credential");

        let deleted = sqlx::query("DELETE FROM integrations.user_clickup_credentials")
            .execute(&mut *tx)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(
            deleted, 0,
            "no one else may delete another user's credential"
        );
    }

    // And a user may not write a row on someone else's behalf.
    let mut tx = begin_rls_transaction(&app, other, &[]).await.unwrap();
    let forged = sqlx::query(
        "INSERT INTO integrations.user_clickup_credentials (user_id, token_ciphertext)
         VALUES ($1, '\\x00'::bytea)",
    )
    .bind(owner)
    .execute(&mut *tx)
    .await;
    assert!(forged.is_err());
}

/// A mock ClickUp that accepts exactly the token "pk_good", and only
/// while `accepting` is true -- flipping it simulates the user revoking
/// or regenerating their token inside ClickUp.
async fn spawn_clickup(accepting: Arc<AtomicBool>) -> String {
    async fn authorize(headers: &HeaderMap, accepting: &AtomicBool) -> bool {
        accepting.load(Ordering::SeqCst)
            && headers.get("authorization").and_then(|v| v.to_str().ok()) == Some("pk_good")
    }

    let user_flag = accepting.clone();
    let team_flag = accepting;

    let app = Router::new()
        .route(
            "/user",
            get(move |headers: HeaderMap| {
                let flag = user_flag.clone();
                async move {
                    if authorize(&headers, &flag).await {
                        Ok(Json(
                            json!({ "user": { "id": 42, "username": "Test Person" } }),
                        ))
                    } else {
                        Err(StatusCode::UNAUTHORIZED)
                    }
                }
            }),
        )
        .route(
            "/team",
            get(move |headers: HeaderMap| {
                let flag = team_flag.clone();
                async move {
                    if authorize(&headers, &flag).await {
                        Ok(Json(
                            json!({ "teams": [ { "id": "1", "name": "QuikStor" } ] }),
                        ))
                    } else {
                        Err(StatusCode::UNAUTHORIZED)
                    }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_db_connection_lifecycle_save_test_invalidate_recover_remove() {
    let _ = dotenvy::from_filename(".env.local");
    std::env::set_var("INTEGRATION_SECRETS_ENCRYPTION_KEY", ENCRYPTION_KEY);

    let accepting = Arc::new(AtomicBool::new(true));
    let base_url = spawn_clickup(accepting.clone()).await;

    let superuser = superuser_pool();
    let user_id = create_user(&superuser, "lifecycle").await;

    let state = AppState {
        db: crate::db::connect_test(),
        env_source: Arc::new(FakeEnvSource::with(&[("CLICKUP_API_BASE_URL", &base_url)])),
        ..empty_state()
    };
    let user = || caller(user_id, &[], &["integrations.clickup"]);

    let save = |token: &str| {
        clickup_connection::save_token(
            State(state.clone()),
            user(),
            local_addr(),
            HeaderMap::new(),
            Json(clickup_connection::SaveTokenRequest {
                token: token.to_string(),
            }),
        )
    };

    // Nothing saved yet.
    let initial = clickup_connection::get_connection(State(state.clone()), user()).await;
    assert_eq!(body_json(initial).await["status"], "not_connected");

    // A token ClickUp rejects is refused and NOT stored.
    let bad = save("pk_bad").await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
    let still_empty = clickup_connection::get_connection(State(state.clone()), user()).await;
    assert_eq!(body_json(still_empty).await["status"], "not_connected");

    // A good token connects and reports who it belongs to.
    let good = save("pk_good").await;
    assert_eq!(good.status(), StatusCode::OK);
    let good_body = body_json(good).await;
    assert_eq!(good_body["status"], "connected");
    assert_eq!(good_body["clickup_username"], "Test Person");
    assert_eq!(good_body["workspace_names"], json!(["QuikStor"]));
    assert!(
        !good_body.to_string().contains("pk_good"),
        "the response must never echo the token"
    );

    // The stored blob is ciphertext, not the token.
    let stored: Vec<u8> = sqlx::query_scalar(
        "SELECT token_ciphertext FROM integrations.user_clickup_credentials WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains("pk_good"));

    // The user revokes the token inside ClickUp: testing flips it to
    // invalid (the nav dot goes red) but keeps the row.
    accepting.store(false, Ordering::SeqCst);
    let invalidated = clickup_connection::test_connection(
        State(state.clone()),
        user(),
        local_addr(),
        HeaderMap::new(),
    )
    .await;
    assert_eq!(body_json(invalidated).await["status"], "invalid");
    let after_invalid = clickup_connection::get_connection(State(state.clone()), user()).await;
    assert_eq!(body_json(after_invalid).await["status"], "invalid");

    // ClickUp working again for the same token: re-testing recovers it.
    accepting.store(true, Ordering::SeqCst);
    let recovered = clickup_connection::test_connection(
        State(state.clone()),
        user(),
        local_addr(),
        HeaderMap::new(),
    )
    .await;
    assert_eq!(body_json(recovered).await["status"], "connected");

    // Removing disconnects; removing again is a harmless no-op.
    for _ in 0..2 {
        let removed = clickup_connection::remove_token(
            State(state.clone()),
            user(),
            local_addr(),
            HeaderMap::new(),
        )
        .await;
        assert_eq!(body_json(removed).await["status"], "not_connected");
    }

    // Testing with nothing saved is a clean 404, not a crash.
    let nothing = clickup_connection::test_connection(
        State(state.clone()),
        user(),
        local_addr(),
        HeaderMap::new(),
    )
    .await;
    assert_eq!(nothing.status(), StatusCode::NOT_FOUND);

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn clickup_db_grant_endpoints_round_trip_and_are_idempotent() {
    let _ = dotenvy::from_filename(".env.local");
    let superuser = superuser_pool();

    let admin_id = create_user(&superuser, "grantor").await;
    let target = create_user(&superuser, "grantee").await;

    let state = AppState {
        db: crate::db::connect_test(),
        ..empty_state()
    };
    let admin = || caller(admin_id, &["admin"], &["user_permissions.manage"]);

    let grant = |key: &str| {
        auth_user_permissions::grant_user_permission(
            State(state.clone()),
            admin(),
            local_addr(),
            HeaderMap::new(),
            Path((target, key.to_string())),
        )
    };

    let state_of = |body: &Value, key: &str| -> bool {
        body["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["key"] == key)
            .map(|p| p["granted"].as_bool().unwrap())
            .unwrap()
    };

    // Listed as grantable, not yet held.
    let listed = auth_user_permissions::list_user_permissions(
        State(state.clone()),
        admin(),
        local_addr(),
        HeaderMap::new(),
        Path(target),
    )
    .await;
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = body_json(listed).await;
    assert!(!state_of(&listed, "integrations.clickup"));
    // Only directly-grantable permissions are ever offered.
    assert!(listed["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|p| p["key"] != "users.manage_roles"));

    // Grant twice: both succeed, state is granted.
    for _ in 0..2 {
        let response = grant("integrations.clickup").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(state_of(&body_json(response).await, "integrations.clickup"));
    }

    // A role-only permission is refused as not grantable.
    let refused = grant("users.manage_roles").await;
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);

    // Revoke twice: both succeed, state is not granted.
    for _ in 0..2 {
        let response = auth_user_permissions::revoke_user_permission(
            State(state.clone()),
            admin(),
            local_addr(),
            HeaderMap::new(),
            Path((target, "integrations.clickup".to_string())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!state_of(
            &body_json(response).await,
            "integrations.clickup"
        ));
    }

    // An unknown user is a clean 404.
    let missing = auth_user_permissions::list_user_permissions(
        State(state.clone()),
        admin(),
        local_addr(),
        HeaderMap::new(),
        Path(Uuid::new_v4()),
    )
    .await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see module doc"]
async fn clickup_db_a_department_manager_can_list_users_and_grant_but_not_manage_them() {
    let _ = dotenvy::from_filename(".env.local");
    let superuser = superuser_pool();

    let manager_id = create_user(&superuser, "deptmgr").await;
    let target = create_user(&superuser, "deptmgr-target").await;

    let state = AppState {
        db: crate::db::connect_test(),
        ..empty_state()
    };
    // Exactly what the department_manager role carries for this feature
    // (see let_department_managers_grant_permissions) -- deliberately
    // neither users.manage nor users.manage_roles.
    let manager = || {
        caller(
            manager_id,
            &["department_manager"],
            &["users.view", "user_permissions.manage"],
        )
    };

    // They can see the Users list...
    let listed = crate::api::auth_users::list_users(State(state.clone()), manager()).await;
    assert_eq!(listed.status(), StatusCode::OK);
    let listed = body_json(listed).await;
    assert!(listed["users"]
        .as_array()
        .unwrap()
        .iter()
        .any(|u| u["id"] == target.to_string()));

    // ...and grant/revoke a personal-integration permission. Their own
    // lookup of the target works only because of auth.user_exists --
    // auth.users itself is invisible to them under RLS.
    let granted = auth_user_permissions::grant_user_permission(
        State(state.clone()),
        manager(),
        local_addr(),
        HeaderMap::new(),
        Path((target, "integrations.clickup".to_string())),
    )
    .await;
    assert_eq!(granted.status(), StatusCode::OK);

    let held: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM auth.user_permissions
                         WHERE user_id = $1 AND permission_key = 'integrations.clickup')",
    )
    .bind(target)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert!(held);

    // They still cannot export the user list...
    let exported = crate::api::auth_users::export_users(State(state.clone()), manager()).await;
    assert_eq!(exported.status(), StatusCode::FORBIDDEN);

    // ...nor grant a role (needs users.manage_roles, which they lack).
    let role = crate::api::auth_user_role::grant_role(
        State(state.clone()),
        manager(),
        local_addr(),
        HeaderMap::new(),
        Path(target),
        Json(crate::api::auth_user_role::GrantRoleRequest {
            role: "admin".to_string(),
        }),
    )
    .await;
    assert_eq!(role.status(), StatusCode::FORBIDDEN);

    // And a role-only permission still cannot be handed out by them.
    let escalation = auth_user_permissions::grant_user_permission(
        State(state.clone()),
        manager(),
        local_addr(),
        HeaderMap::new(),
        Path((target, "users.manage_roles".to_string())),
    )
    .await;
    assert_eq!(escalation.status(), StatusCode::BAD_REQUEST);
}
