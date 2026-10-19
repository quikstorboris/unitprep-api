//! Real-database tests for invite creation and admin account recovery
//! (`auth_invites`). The invite *redemption* side (expiry, single use,
//! consumption in the same transaction as the credential) is already
//! covered by `auth_register_db_tests`; this covers the issuing side, which
//! is where a bearer credential is minted and where role assignment at
//! invite time could bypass `users.manage_roles`.
//!
//! Every test is `#[ignore]`d and `*_db_*`-named, so CI's ephemeral
//! Postgres picks them up automatically. See `clickup_db_tests` for setup.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Json, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::auth_invites::{
    create_invite, recover_account, CreateInviteRequest, RecoverAccountRequest,
};
use super::clickup_db_tests::{body_json, caller, create_user, superuser_pool};
use crate::api::test_support::empty_state;
use crate::api::AppState;
use crate::auth::{audit_log, hash_token, AuthenticatedUser};

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

fn unique_email(label: &str) -> String {
    format!("{label}-{}@example.test", Uuid::new_v4())
}

fn invite_body(email: &str, role: &str) -> CreateInviteRequest {
    CreateInviteRequest {
        email: email.to_string(),
        first_name: "Ada".to_string(),
        last_name: "Lovelace".to_string(),
        company: "quikstor".to_string(),
        job_title: None,
        role: role.to_string(),
    }
}

async fn invite(actor: AuthenticatedUser, body: CreateInviteRequest) -> Response {
    create_invite(State(state()), actor, peer(), HeaderMap::new(), Json(body)).await
}

async fn recover(actor: AuthenticatedUser, email: &str) -> Response {
    recover_account(
        State(state()),
        actor,
        peer(),
        HeaderMap::new(),
        Json(RecoverAccountRequest {
            email: email.to_string(),
        }),
    )
    .await
}

async fn error_code(response: Response) -> String {
    body_json(response).await["error"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// A real actor row, so audit rows referencing the admin satisfy their FK.
async fn real_admin(superuser: &PgPool) -> AuthenticatedUser {
    let id = create_user(superuser, "inviter").await;
    caller(id, &["admin"], &["users.manage", "users.manage_roles"])
}

async fn user_by_email(superuser: &PgPool, email: &str) -> Option<(Uuid, String)> {
    sqlx::query_as(
        "SELECT id, status::text FROM auth.users WHERE email = $1::citext AND deleted_at IS NULL",
    )
    .bind(email)
    .fetch_optional(superuser)
    .await
    .unwrap()
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

async fn live_invites(superuser: &PgPool, user_id: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM auth.user_invites
          WHERE user_id = $1 AND used_at IS NULL AND expires_at > now()",
    )
    .bind(user_id)
    .fetch_one(superuser)
    .await
    .unwrap()
}

async fn token_is_live(superuser: &PgPool, raw_token: &str) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM auth.user_invites
                        WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now())",
    )
    .bind(hash_token(raw_token))
    .fetch_one(superuser)
    .await
    .unwrap()
}

async fn audit_for(superuser: &PgPool, event: &str, target: Uuid) -> Vec<(Value, Value, Value)> {
    sqlx::query_as(
        "SELECT coalesce(before_state, 'null'::jsonb), coalesce(after_state, 'null'::jsonb), metadata
           FROM auth.auth_audit_logs WHERE event_type = $1 AND target_user_id = $2",
    )
    .bind(event)
    .bind(target)
    .fetch_all(superuser)
    .await
    .unwrap()
}

/// An `invited` account with one live invite, as `create_invite` leaves it.
async fn invited_account(superuser: &PgPool, email: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO auth.users (id, email, first_name, last_name, company, status)
         VALUES ($1, $2::citext, 'Old', 'Invitee', 'quikstor', 'invited')",
    )
    .bind(id)
    .bind(email)
    .execute(superuser)
    .await
    .unwrap();
    id
}

// -------------------------------------------------------- create_invite

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn invites_db_a_new_address_creates_an_invited_account_with_the_role_and_a_hashed_token() {
    let superuser = superuser_pool();
    let actor = real_admin(&superuser).await;
    let email = unique_email("new-hire");

    let response = invite(
        actor,
        invite_body(
            &format!("  {}  ", email.to_uppercase()),
            "Onboarding_Manager",
        ),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    assert_eq!(body["reissued"], false);
    let token = body["invite_token"].as_str().unwrap().to_string();

    let (user_id, status) = user_by_email(&superuser, &email)
        .await
        .expect("the address is normalised to lower case and trimmed");
    assert_eq!(status, "invited");
    assert_eq!(roles_of(&superuser, user_id).await, ["onboarding_manager"]);
    assert!(token_is_live(&superuser, &token).await);

    // Only the hash is stored; the raw token must not be anywhere in the DB
    // audit trail.
    let rows = audit_for(&superuser, audit_log::event::INVITE_CREATED, user_id).await;
    assert_eq!(rows.len(), 1);
    assert!(
        !rows[0].2.to_string().contains(&token),
        "the bearer token must never be written to the audit log"
    );
    assert_eq!(rows[0].2["role"], "onboarding_manager");
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn invites_db_reissuing_retires_the_old_link_and_replaces_the_role() {
    let superuser = superuser_pool();
    let actor = real_admin(&superuser).await;
    let email = unique_email("reissue");

    let first = body_json(
        invite(
            real_admin_clone(&actor),
            invite_body(&email, "onboarding_manager"),
        )
        .await,
    )
    .await;
    let old_token = first["invite_token"].as_str().unwrap().to_string();

    let second = invite(real_admin_clone(&actor), invite_body(&email, "developer")).await;
    assert_eq!(second.status(), StatusCode::CREATED);
    let second = body_json(second).await;
    assert_eq!(second["reissued"], true);
    let new_token = second["invite_token"].as_str().unwrap().to_string();

    let (user_id, _) = user_by_email(&superuser, &email).await.unwrap();
    assert!(
        !token_is_live(&superuser, &old_token).await,
        "a lost link must die on reissue"
    );
    assert!(token_is_live(&superuser, &new_token).await);
    assert_eq!(live_invites(&superuser, user_id).await, 1);
    assert_eq!(roles_of(&superuser, user_id).await, ["developer"]);
}

fn real_admin_clone(user: &AuthenticatedUser) -> AuthenticatedUser {
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

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn invites_db_an_already_active_account_is_refused_and_the_refusal_is_audited() {
    let superuser = superuser_pool();
    let actor = real_admin(&superuser).await;
    let existing = create_user(&superuser, "already-active").await;
    let email: String = sqlx::query_scalar("SELECT email::text FROM auth.users WHERE id = $1")
        .bind(existing)
        .fetch_one(&superuser)
        .await
        .unwrap();

    let response = invite(actor, invite_body(&email, "admin")).await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(response).await, "invite_not_applicable");
    let rows = audit_for(&superuser, audit_log::event::INVITE_REFUSED, existing).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2["reason"], "not_invited_status");
    assert_eq!(live_invites(&superuser, existing).await, 0);
}

/// KNOWN GAP (named `known_gap` so `run_ci_db_tests.sh` skips it, since the
/// module name alone matches its `_db_` filter). `issue_invite` refuses an
/// account that already has passkeys by counting `auth.webauthn_credentials`
/// rows, but that table's only RLS policy is owner-only
/// (`webauthn_credentials_owner_only`), so inside the admin's transaction the
/// count for ANOTHER user is always 0 and the `already_credentialed` refusal
/// can never fire. The `status != 'invited'` check right after it still
/// blocks every real case (registering a passkey activates the account), so
/// this is a dead guard rather than an open hole. Remove the `#[ignore]` once
/// the count goes through a SECURITY DEFINER lookup.
#[tokio::test]
#[ignore = "known gap: credential-count guard is blind to other users' passkeys under RLS"]
async fn invites_known_gap_an_account_that_already_has_a_passkey_cannot_be_reinvited() {
    let superuser = superuser_pool();
    let actor = real_admin(&superuser).await;
    let email = unique_email("enrolled");
    let id = invited_account(&superuser, &email).await;
    sqlx::query(
        "INSERT INTO auth.webauthn_credentials (user_id, credential_id, passkey_data)
         VALUES ($1, $2, $3)",
    )
    .bind(id)
    .bind(Uuid::new_v4().as_bytes().to_vec())
    .bind(serde_json::json!({ "fixture": true }))
    .execute(&superuser)
    .await
    .unwrap();

    let response = invite(actor, invite_body(&email, "admin")).await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    let rows = audit_for(&superuser, audit_log::event::INVITE_REFUSED, id).await;
    assert_eq!(rows[0].2["reason"], "already_credentialed");
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn invites_db_an_unknown_role_creates_nothing() {
    let superuser = superuser_pool();
    let actor = real_admin(&superuser).await;
    let email = unique_email("bad-role");

    let response = invite(actor, invite_body(&email, "superuser")).await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(response).await, "invalid_role");
    assert!(user_by_email(&superuser, &email).await.is_none());
}

/// The reason `create_invite` demands `users.manage_roles` too: a role that
/// can invite people must not be able to invite someone straight in as
/// admin.
#[tokio::test]
#[ignore = "needs the local test-db"]
async fn invites_db_users_manage_alone_cannot_invite_with_a_role() {
    let superuser = superuser_pool();
    let id = create_user(&superuser, "inviter-lite").await;
    let actor = caller(id, &["admin"], &["users.manage"]);
    let email = unique_email("escalation");

    let response = invite(actor, invite_body(&email, "admin")).await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(user_by_email(&superuser, &email).await.is_none());
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn invites_db_the_database_refuses_an_invite_from_a_non_admin_even_with_both_permissions() {
    let superuser = superuser_pool();
    let id = create_user(&superuser, "impostor").await;
    let actor = caller(
        id,
        &["onboarding_manager"],
        &["users.manage", "users.manage_roles"],
    );
    let email = unique_email("blocked");

    let response = invite(actor, invite_body(&email, "admin")).await;

    assert_ne!(response.status(), StatusCode::CREATED);
    assert!(
        user_by_email(&superuser, &email).await.is_none(),
        "a non-admin must never be able to mint an account"
    );
}

/// KNOWN GAP, deliberately not named `*_db_*` so CI's `--ignored` sweep does
/// not pick it up. `issue_invite` documents that a soft-deleted account's
/// address "can be re-invited rather than being permanently unusable", but
/// `auth.users.email` carries a plain UNIQUE constraint (`users_email_key`),
/// not one scoped to `deleted_at IS NULL`, so the INSERT collides and the
/// endpoint answers 500. Nothing in the app soft-deletes a user today, so it
/// has not bitten; remove the `#[ignore]` once the schema or handler agrees
/// with the comment.
#[tokio::test]
#[ignore = "known gap: soft-deleted address cannot be re-invited (unique email constraint)"]
async fn invites_known_gap_soft_deleted_address_can_be_invited_again() {
    let superuser = superuser_pool();
    let actor = real_admin(&superuser).await;
    let email = unique_email("returning");
    let old = invited_account(&superuser, &email).await;
    sqlx::query(
        "UPDATE auth.users SET deleted_at = now(), deletion_reason = 'offboarding' WHERE id = $1",
    )
    .bind(old)
    .execute(&superuser)
    .await
    .unwrap();

    let response = invite(actor, invite_body(&email, "onboarding_manager")).await;

    assert_eq!(response.status(), StatusCode::CREATED);
    let (new_id, status) = user_by_email(&superuser, &email).await.unwrap();
    assert_ne!(new_id, old, "a fresh account, not a resurrected one");
    assert_eq!(status, "invited");
}

// ------------------------------------------------------ recover_account

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn recovery_db_wipes_every_access_path_and_leaves_one_fresh_invite() {
    let superuser = superuser_pool();
    let actor = real_admin(&superuser).await;
    let target = create_user(&superuser, "lost-device").await;
    let email: String = sqlx::query_scalar("SELECT email::text FROM auth.users WHERE id = $1")
        .bind(target)
        .fetch_one(&superuser)
        .await
        .unwrap();
    let (_, session_hash) = crate::auth::generate_token();
    sqlx::query(
        "INSERT INTO auth.sessions (user_id, token_hash, expires_at)
         VALUES ($1, $2, now() + interval '1 hour')",
    )
    .bind(target)
    .bind(&session_hash)
    .execute(&superuser)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO auth.webauthn_credentials (user_id, credential_id, passkey_data)
         VALUES ($1, $2, $3)",
    )
    .bind(target)
    .bind(Uuid::new_v4().as_bytes().to_vec())
    .bind(serde_json::json!({ "fixture": true }))
    .execute(&superuser)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO auth.totp_credentials (user_id, secret_encrypted, confirmed_at)
         VALUES ($1, $2, now())",
    )
    .bind(target)
    .bind(vec![1u8, 2])
    .execute(&superuser)
    .await
    .unwrap();

    let response = recover(actor, &format!(" {} ", email.to_uppercase())).await;

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = body_json(response).await;
    let token = body["invite_token"].as_str().unwrap();
    assert_eq!(body["reissued"], true);

    let (status,): (String,) = sqlx::query_as("SELECT status::text FROM auth.users WHERE id = $1")
        .bind(target)
        .fetch_one(&superuser)
        .await
        .unwrap();
    assert_eq!(status, "invited");
    let passkeys: i64 =
        sqlx::query_scalar("SELECT count(*) FROM auth.webauthn_credentials WHERE user_id = $1")
            .bind(target)
            .fetch_one(&superuser)
            .await
            .unwrap();
    let totp: i64 =
        sqlx::query_scalar("SELECT count(*) FROM auth.totp_credentials WHERE user_id = $1")
            .bind(target)
            .fetch_one(&superuser)
            .await
            .unwrap();
    let live_sessions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM auth.sessions WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(target)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert_eq!((passkeys, totp, live_sessions), (0, 0, 0));
    assert!(token_is_live(&superuser, token).await);
    assert_eq!(live_invites(&superuser, target).await, 1);

    let rows = audit_for(
        &superuser,
        audit_log::event::ACCOUNT_RECOVERY_INITIATED,
        target,
    )
    .await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0["status"], "active");
    assert_eq!(rows[0].1["status"], "invited");
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn recovery_db_refuses_an_account_that_never_enrolled_or_is_deactivated() {
    let superuser = superuser_pool();
    let actor = real_admin(&superuser).await;

    let invited_email = unique_email("not-enrolled");
    let invited = invited_account(&superuser, &invited_email).await;
    let response = recover(real_admin_clone(&actor), &invited_email).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        audit_for(&superuser, audit_log::event::INVITE_REFUSED, invited).await[0].2["reason"],
        "not_yet_enrolled"
    );

    let deactivated = create_user(&superuser, "deactivated").await;
    sqlx::query("SELECT auth.set_user_status($1, 'deactivated'::auth.user_status)")
        .bind(deactivated)
        .execute(&superuser)
        .await
        .unwrap();
    let email: String = sqlx::query_scalar("SELECT email::text FROM auth.users WHERE id = $1")
        .bind(deactivated)
        .fetch_one(&superuser)
        .await
        .unwrap();
    let response = recover(real_admin_clone(&actor), &email).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        audit_for(&superuser, audit_log::event::INVITE_REFUSED, deactivated).await[0].2["reason"],
        "account_deactivated"
    );
    assert_eq!(live_invites(&superuser, deactivated).await, 0);
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn recovery_db_an_unknown_address_is_a_404_and_a_malformed_one_a_400() {
    let superuser = superuser_pool();
    let actor = real_admin(&superuser).await;

    let missing = recover(real_admin_clone(&actor), &unique_email("nobody")).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    assert_eq!(error_code(missing).await, "account_not_found");

    let malformed = recover(real_admin_clone(&actor), "not-an-address").await;
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
#[ignore = "needs the local test-db"]
async fn recovery_db_the_database_refuses_a_non_admin_even_with_the_permission() {
    let superuser = superuser_pool();
    let target = create_user(&superuser, "protected").await;
    let email: String = sqlx::query_scalar("SELECT email::text FROM auth.users WHERE id = $1")
        .bind(target)
        .fetch_one(&superuser)
        .await
        .unwrap();
    let impostor = caller(
        create_user(&superuser, "impostor").await,
        &["onboarding_manager"],
        &["users.manage"],
    );

    let response = recover(impostor, &email).await;

    assert_ne!(response.status(), StatusCode::CREATED);
    let status: String = sqlx::query_scalar("SELECT status::text FROM auth.users WHERE id = $1")
        .bind(target)
        .fetch_one(&superuser)
        .await
        .unwrap();
    assert_eq!(
        status, "active",
        "an impostor must not be able to lock someone out"
    );
}
