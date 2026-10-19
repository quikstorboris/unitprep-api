//! Real-database tests for the admin actions on another user's account:
//! grant/revoke a role (`auth_user_role`) and deactivate/reactivate
//! (`auth_user_status`). This is the area of the 2026-07-30 role-escalation
//! bug, so the matrix covers who may change whom, the last-admin guard,
//! the "never yourself" rule, the audit row each change must leave, and --
//! the part offline tests cannot prove -- that the database refuses the
//! change on its own when the application-level permission check is
//! bypassed.
//!
//! Every test is `#[ignore]`d and named `*_db_*`, so `run_ci_db_tests.sh`
//! picks them up automatically. Fixtures are written as the local
//! superuser; everything under test runs as `app_service` through
//! `begin_rls_transaction`, so RLS genuinely applies (see
//! `clickup_db_tests`' module doc).
//!
//! The two last-admin tests count admins across the WHOLE database, which
//! other tests also populate. They therefore run under one `serial` key
//! and temporarily soft-delete every other admin account, restoring them
//! before asserting.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Json, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::auth_user_role::{grant_role, revoke_role, GrantRoleRequest};
use super::auth_user_status::{deactivate_user, reactivate_user};
use super::clickup_db_tests::{body_json, caller, create_user, superuser_pool};
use crate::api::test_support::empty_state;
use crate::api::AppState;
use crate::auth::{audit_log, hash_token};

fn state() -> AppState {
    let _ = dotenvy::from_filename(".env.local");
    AppState {
        db: crate::db::connect_test(),
        ..empty_state()
    }
}

fn peer() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
}

/// An admin as the handlers see one: the permissions the admin role
/// carries, and the `admin` role key RLS keys on.
fn admin(user_id: Uuid) -> crate::auth::AuthenticatedUser {
    caller(user_id, &["admin"], &["users.manage", "users.manage_roles"])
}

async fn give_role(superuser: &PgPool, user_id: Uuid, role_key: &str) {
    sqlx::query(
        "INSERT INTO auth.user_roles (user_id, role_id)
         SELECT $1, id FROM auth.roles WHERE key = $2",
    )
    .bind(user_id)
    .bind(role_key)
    .execute(superuser)
    .await
    .expect("fixture role grant must insert");
}

async fn roles_of(superuser: &PgPool, user_id: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT r.key FROM auth.user_roles ur JOIN auth.roles r ON r.id = ur.role_id
          WHERE ur.user_id = $1 ORDER BY r.key",
    )
    .bind(user_id)
    .fetch_all(superuser)
    .await
    .unwrap()
}

async fn status_of(superuser: &PgPool, user_id: Uuid) -> String {
    sqlx::query_scalar("SELECT status::text FROM auth.users WHERE id = $1")
        .bind(user_id)
        .fetch_one(superuser)
        .await
        .unwrap()
}

async fn audit_rows(superuser: &PgPool, event: &str, actor: Uuid, target: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM auth.auth_audit_logs
          WHERE event_type = $1 AND actor_user_id = $2 AND target_user_id = $3",
    )
    .bind(event)
    .bind(actor)
    .bind(target)
    .fetch_one(superuser)
    .await
    .unwrap()
}

async fn do_grant(actor: crate::auth::AuthenticatedUser, target: Uuid, role: &str) -> Response {
    grant_role(
        State(state()),
        actor,
        peer(),
        HeaderMap::new(),
        Path(target),
        Json(GrantRoleRequest {
            role: role.to_string(),
        }),
    )
    .await
}

async fn do_revoke(actor: crate::auth::AuthenticatedUser, target: Uuid, role: &str) -> Response {
    revoke_role(
        State(state()),
        actor,
        peer(),
        HeaderMap::new(),
        Path((target, role.to_string())),
    )
    .await
}

async fn do_deactivate(actor: crate::auth::AuthenticatedUser, target: Uuid) -> Response {
    deactivate_user(
        State(state()),
        actor,
        peer(),
        HeaderMap::new(),
        Path(target),
    )
    .await
}

async fn do_reactivate(actor: crate::auth::AuthenticatedUser, target: Uuid) -> Response {
    reactivate_user(
        State(state()),
        actor,
        peer(),
        HeaderMap::new(),
        Path(target),
    )
    .await
}

async fn error_code(response: Response) -> String {
    let body: Value = body_json(response).await;
    body["error"].as_str().unwrap_or_default().to_string()
}

/// Soft-deletes every active admin except `keep`, returning their ids so
/// the caller can restore them. Run only under the `admin_population`
/// serial key.
async fn hide_other_admins(superuser: &PgPool, keep: &[Uuid]) -> Vec<Uuid> {
    sqlx::query_scalar(
        "UPDATE auth.users u SET deleted_at = now(), deletion_reason = 'offboarding'
          WHERE u.deleted_at IS NULL
            AND u.id <> ALL($1)
            AND EXISTS (SELECT 1 FROM auth.user_roles ur JOIN auth.roles r ON r.id = ur.role_id
                         WHERE ur.user_id = u.id AND r.key = 'admin')
        RETURNING u.id",
    )
    .bind(keep)
    .fetch_all(superuser)
    .await
    .unwrap()
}

async fn restore_admins(superuser: &PgPool, ids: &[Uuid]) {
    sqlx::query(
        "UPDATE auth.users SET deleted_at = NULL, deletion_reason = NULL WHERE id = ANY($1)",
    )
    .bind(ids)
    .execute(superuser)
    .await
    .unwrap();
}

// ---------------------------------------------------------------- roles

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn user_admin_db_granting_a_role_adds_it_and_writes_an_audit_row_with_before_and_after() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "grantor").await;
    let target = create_user(&superuser, "grantee").await;

    let response = do_grant(admin(actor), target, "  Onboarding_Manager ").await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["roles"], serde_json::json!(["onboarding_manager"]));
    assert_eq!(roles_of(&superuser, target).await, ["onboarding_manager"]);

    let (before, after): (Value, Value) = sqlx::query_as(
        "SELECT before_state, after_state FROM auth.auth_audit_logs
          WHERE event_type = $1 AND actor_user_id = $2 AND target_user_id = $3",
    )
    .bind(audit_log::event::ROLE_GRANTED)
    .bind(actor)
    .bind(target)
    .fetch_one(&superuser)
    .await
    .expect("exactly one audit row for the grant");
    assert_eq!(before["roles"], serde_json::json!([]));
    assert_eq!(after["roles"], serde_json::json!(["onboarding_manager"]));
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn user_admin_db_granting_a_role_already_held_is_a_conflict_and_leaves_no_audit_row() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "grantor").await;
    let target = create_user(&superuser, "grantee").await;
    give_role(&superuser, target, "onboarding_manager").await;

    let response = do_grant(admin(actor), target, "onboarding_manager").await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(response).await, "role_already_held");
    assert_eq!(
        audit_rows(&superuser, audit_log::event::ROLE_GRANTED, actor, target).await,
        0
    );
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn user_admin_db_granting_an_unknown_role_or_to_a_missing_user_is_refused() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "grantor").await;
    let target = create_user(&superuser, "grantee").await;

    let unknown_role = do_grant(admin(actor), target, "superuser").await;
    assert_eq!(unknown_role.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(unknown_role).await, "invalid_role");
    assert!(roles_of(&superuser, target).await.is_empty());

    let missing_user = do_grant(admin(actor), Uuid::new_v4(), "admin").await;
    assert_eq!(missing_user.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn user_admin_db_a_soft_deleted_user_cannot_be_granted_a_role() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "grantor").await;
    let target = create_user(&superuser, "gone").await;
    sqlx::query(
        "UPDATE auth.users SET deleted_at = now(), deletion_reason = 'offboarding' WHERE id = $1",
    )
    .bind(target)
    .execute(&superuser)
    .await
    .unwrap();

    let response = do_grant(admin(actor), target, "onboarding_manager").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(roles_of(&superuser, target).await.is_empty());
}

/// The escalation class: if the application-level permission check were
/// ever skipped or mis-wired, the database must still refuse. This caller
/// CLAIMS the permission but does not hold the `admin` role the RLS policy
/// requires.
#[tokio::test]
#[ignore = "needs the local test-db"]
async fn user_admin_db_the_database_refuses_a_role_grant_from_a_non_admin_even_with_the_permission()
{
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "impostor").await;
    let target = create_user(&superuser, "victim").await;
    let impostor = caller(actor, &["onboarding_manager"], &["users.manage_roles"]);

    let response = do_grant(impostor, target, "admin").await;

    assert_ne!(response.status(), StatusCode::OK);
    assert!(
        roles_of(&superuser, target).await.is_empty(),
        "a non-admin must never be able to grant admin"
    );
    assert_eq!(
        audit_rows(&superuser, audit_log::event::ROLE_GRANTED, actor, target).await,
        0
    );
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn user_admin_db_the_database_refuses_a_role_revoke_from_a_non_admin_even_with_the_permission(
) {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "impostor").await;
    let target = create_user(&superuser, "victim").await;
    give_role(&superuser, target, "onboarding_manager").await;
    let impostor = caller(actor, &["onboarding_manager"], &["users.manage_roles"]);

    let response = do_revoke(impostor, target, "onboarding_manager").await;

    assert_ne!(response.status(), StatusCode::OK);
    assert_eq!(roles_of(&superuser, target).await, ["onboarding_manager"]);
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn user_admin_db_revoking_a_role_removes_only_that_role_and_audits_it() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "revoker").await;
    let target = create_user(&superuser, "target").await;
    give_role(&superuser, target, "onboarding_manager").await;
    give_role(&superuser, target, "developer").await;

    let response = do_revoke(admin(actor), target, "developer").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(roles_of(&superuser, target).await, ["onboarding_manager"]);
    assert_eq!(
        audit_rows(&superuser, audit_log::event::ROLE_REVOKED, actor, target).await,
        1
    );
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn user_admin_db_revoking_a_role_not_held_is_a_conflict() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "revoker").await;
    let target = create_user(&superuser, "target").await;

    let response = do_revoke(admin(actor), target, "onboarding_manager").await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(response).await, "role_not_held");
    assert_eq!(
        audit_rows(&superuser, audit_log::event::ROLE_REVOKED, actor, target).await,
        0
    );
}

#[tokio::test]
#[serial_test::serial(admin_population)]
#[ignore = "needs the local test-db"]
async fn user_admin_db_the_last_active_admin_cannot_lose_the_admin_role() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "operator").await;
    give_role(&superuser, actor, "admin").await;
    let sole_admin = create_user(&superuser, "sole-admin").await;
    give_role(&superuser, sole_admin, "admin").await;
    let hidden = hide_other_admins(&superuser, &[actor, sole_admin]).await;
    // The operator is an admin too, so `sole_admin` is NOT the last one:
    // take the operator out of the count by making them non-active.
    sqlx::query(
        "UPDATE auth.users SET deleted_at = now(), deletion_reason = 'offboarding' WHERE id = $1",
    )
    .bind(actor)
    .execute(&superuser)
    .await
    .unwrap();
    // The handler only needs the caller's claimed role/permissions, not a
    // live account, so the operator can act while excluded from the count.

    let response = do_revoke(admin(actor), sole_admin, "admin").await;

    sqlx::query("UPDATE auth.users SET deleted_at = NULL, deletion_reason = NULL WHERE id = $1")
        .bind(actor)
        .execute(&superuser)
        .await
        .unwrap();
    restore_admins(&superuser, &hidden).await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(response).await, "last_active_admin");
    assert_eq!(roles_of(&superuser, sole_admin).await, ["admin"]);
}

#[tokio::test]
#[serial_test::serial(admin_population)]
#[ignore = "needs the local test-db"]
async fn user_admin_db_an_admin_role_can_be_revoked_when_another_active_admin_remains() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "operator").await;
    give_role(&superuser, actor, "admin").await;
    let first = create_user(&superuser, "admin-one").await;
    give_role(&superuser, first, "admin").await;

    let response = do_revoke(admin(actor), first, "admin").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(roles_of(&superuser, first).await.is_empty());
}

// ---------------------------------------------------------------- status

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn user_admin_db_deactivating_a_user_flips_status_and_audits_it() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "deactivator").await;
    let target = create_user(&superuser, "leaver").await;

    let response = do_deactivate(admin(actor), target).await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(status_of(&superuser, target).await, "deactivated");
    assert_eq!(
        audit_rows(
            &superuser,
            audit_log::event::USER_DEACTIVATED,
            actor,
            target
        )
        .await,
        1
    );
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn user_admin_db_deactivating_revokes_the_targets_live_sessions() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "deactivator").await;
    let target = create_user(&superuser, "leaver").await;
    let (_, token_hash) = crate::auth::generate_token();
    sqlx::query(
        "INSERT INTO auth.sessions (user_id, token_hash, expires_at)
         VALUES ($1, $2, now() + interval '1 hour')",
    )
    .bind(target)
    .bind(&token_hash)
    .execute(&superuser)
    .await
    .unwrap();

    let response = do_deactivate(admin(actor), target).await;
    assert_eq!(response.status(), StatusCode::OK);

    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM auth.sessions
          WHERE user_id = $1 AND revoked_at IS NULL AND expires_at > now()",
    )
    .bind(target)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert_eq!(live, 0, "a deactivated user must not keep a usable session");
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn user_admin_db_deactivating_twice_missing_users_and_the_database_gate() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "deactivator").await;
    let target = create_user(&superuser, "leaver").await;
    assert_eq!(
        do_deactivate(admin(actor), target).await.status(),
        StatusCode::OK
    );

    let again = do_deactivate(admin(actor), target).await;
    assert_eq!(again.status(), StatusCode::CONFLICT);

    let missing = do_deactivate(admin(actor), Uuid::new_v4()).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // A non-admin claiming the permission is stopped by the database.
    let other = create_user(&superuser, "bystander").await;
    let impostor = caller(
        create_user(&superuser, "impostor").await,
        &["onboarding_manager"],
        &["users.manage"],
    );
    let response = do_deactivate(impostor, other).await;
    assert_ne!(response.status(), StatusCode::OK);
    assert_eq!(status_of(&superuser, other).await, "active");
}

#[tokio::test]
#[serial_test::serial(admin_population)]
#[ignore = "needs the local test-db"]
async fn user_admin_db_the_last_active_admin_cannot_be_deactivated() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "operator").await;
    let sole_admin = create_user(&superuser, "sole-admin").await;
    give_role(&superuser, sole_admin, "admin").await;
    let hidden = hide_other_admins(&superuser, &[sole_admin]).await;

    let response = do_deactivate(admin(actor), sole_admin).await;

    restore_admins(&superuser, &hidden).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(status_of(&superuser, sole_admin).await, "active");
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn user_admin_db_reactivating_returns_the_user_to_invited_with_a_working_one_time_invite() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "reactivator").await;
    let target = create_user(&superuser, "returner").await;
    assert_eq!(
        do_deactivate(admin(actor), target).await.status(),
        StatusCode::OK
    );

    let response = do_reactivate(admin(actor), target).await;

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    let token = body["invite_token"].as_str().expect("invite token");
    assert_eq!(status_of(&superuser, target).await, "invited");

    let usable: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM auth.user_invites
          WHERE user_id = $1 AND token_hash = $2 AND expires_at > now() AND used_at IS NULL",
    )
    .bind(target)
    .bind(hash_token(token))
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert_eq!(usable, 1, "the stored invite must match the returned token");
    assert_eq!(
        audit_rows(
            &superuser,
            audit_log::event::USER_REACTIVATED,
            actor,
            target
        )
        .await,
        1
    );
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn user_admin_db_reactivating_a_user_who_is_not_deactivated_or_does_not_exist_is_refused() {
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "reactivator").await;
    let active = create_user(&superuser, "still-active").await;

    let not_deactivated = do_reactivate(admin(actor), active).await;
    assert_eq!(not_deactivated.status(), StatusCode::CONFLICT);
    assert_eq!(status_of(&superuser, active).await, "active");

    let missing = do_reactivate(admin(actor), Uuid::new_v4()).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}
