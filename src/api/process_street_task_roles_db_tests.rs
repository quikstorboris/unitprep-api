//! Real-database tests for the Process Street task-role mapping admin
//! API: the seeded names, replace-all semantics, validation, and that
//! only an admin can write it. Every test is `#[ignore]`d -- local
//! `test-db` only; see `clickup_db_tests`' module doc for how to run
//! them. The mapping is a shared singleton, so each test restores the
//! seeded names before it finishes.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::clickup_db_tests::{body_json, caller, create_user, superuser_pool};
use crate::api::process_street_task_roles::{self, UpdateTaskRoleRequest};
use crate::api::test_support::empty_state;
use crate::api::AppState;

const ROLE: &str = "qms_credentials";

fn state() -> AppState {
    let _ = dotenvy::from_filename(".env.local");
    AppState {
        db: crate::db::connect_test(),
        ..empty_state()
    }
}

async fn put(state: &AppState, admin: Uuid, names: &[&str], role: &str) -> (StatusCode, Value) {
    let response = process_street_task_roles::update_task_role(
        State(state.clone()),
        caller(admin, &["admin"], &["integrations.manage"]),
        HeaderMap::new(),
        Path(role.to_string()),
        Json(UpdateTaskRoleRequest {
            task_names: names.iter().map(|n| n.to_string()).collect(),
        }),
    )
    .await;
    let status = response.status();
    (status, body_json(response).await)
}

async fn restore_seed(superuser: &PgPool) {
    sqlx::query("DELETE FROM integrations.ps_task_role_name WHERE role = $1")
        .bind(ROLE)
        .execute(superuser)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO integrations.ps_task_role_name (role, task_name) VALUES
            ($1, 'Document Credentials'), ($1, 'Add Credentials to QMS')",
    )
    .bind(ROLE)
    .execute(superuser)
    .await
    .unwrap();
}

fn names(role: &Value) -> Vec<String> {
    role["task_names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n.as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
#[serial_test::serial(ps_task_roles)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn task_roles_db_get_lists_the_seeded_names() {
    let state = state();
    let superuser = superuser_pool();
    restore_seed(&superuser).await;
    let admin = create_user(&superuser, "task-roles-get").await;

    let response = process_street_task_roles::get_task_roles(
        State(state),
        caller(admin, &["admin"], &["integrations.manage"]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    let role = &body["roles"][0];
    assert_eq!(role["role"], ROLE);
    assert_eq!(
        names(role),
        vec!["Document Credentials", "Add Credentials to QMS"]
    );
}

#[tokio::test]
#[serial_test::serial(ps_task_roles)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn task_roles_db_put_replaces_the_list_and_normalizes_names() {
    let state = state();
    let superuser = superuser_pool();
    restore_seed(&superuser).await;
    let admin = create_user(&superuser, "task-roles-put").await;

    let (status, role) = put(
        &state,
        admin,
        &[
            "  Document Credentials ",
            "document credentials",
            "Collect Credentials",
        ],
        ROLE,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    // The old "Add Credentials to QMS" name is gone; the duplicate
    // collapsed; the kept name retains its original spelling and order.
    assert_eq!(
        names(&role),
        vec!["Document Credentials", "Collect Credentials"]
    );

    restore_seed(&superuser).await;
}

#[tokio::test]
#[serial_test::serial(ps_task_roles)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn task_roles_db_put_refuses_an_empty_list_and_an_unknown_role() {
    let state = state();
    let superuser = superuser_pool();
    restore_seed(&superuser).await;
    let admin = create_user(&superuser, "task-roles-bad").await;

    let (status, _) = put(&state, admin, &["  ", ""], ROLE).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = put(&state, admin, &["x"], "not_a_role").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The refused writes left the seeded mapping untouched.
    let response = process_street_task_roles::get_task_roles(
        State(state),
        caller(admin, &["admin"], &["integrations.manage"]),
    )
    .await;
    assert_eq!(
        names(&body_json(response).await["roles"][0]),
        vec!["Document Credentials", "Add Credentials to QMS"]
    );
}

#[tokio::test]
#[serial_test::serial(ps_task_roles)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn task_roles_db_only_an_admin_can_write_the_mapping() {
    let state = state();
    let superuser = superuser_pool();
    restore_seed(&superuser).await;
    let manager = create_user(&superuser, "task-roles-manager").await;

    // Even holding the permission key, the table's own RLS write policy
    // is admin-only -- the app-layer check is not the only enforcement.
    let response = process_street_task_roles::update_task_role(
        State(state),
        caller(manager, &["onboarding_manager"], &["integrations.manage"]),
        HeaderMap::new(),
        Path(ROLE.to_string()),
        Json(UpdateTaskRoleRequest {
            task_names: vec!["Sneaky".to_string()],
        }),
    )
    .await;
    assert_ne!(response.status(), StatusCode::OK);

    let remaining: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM integrations.ps_task_role_name WHERE role = $1 AND task_name = 'Sneaky'",
    )
    .bind(ROLE)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert_eq!(remaining, 0);

    restore_seed(&superuser).await;
}
