//! Real-database tests for G1a's two new audit events:
//! `integration_settings_updated` (an admin edits Dropbox / Process Street
//! settings or a task-role mapping) and `activity_log_exported`. Each test
//! is `#[ignore]`d -- local `test-db` only; see `clickup_db_tests` module
//! doc for how to run them.
//!
//! What they pin down: the row exists, names the right actor/integration,
//! carries the caller's IP, records only THAT a secret was replaced (never
//! its value), and a request that is refused or rejected writes nothing.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::clickup_db_tests::{caller, create_user, superuser_pool};
use crate::api::client_ops_activity_logs_export::{
    export_activity_logs, ExportActivityLogsRequest,
};
use crate::api::dropbox_settings::{self, UpdateDropboxSettingsRequest};
use crate::api::process_street_settings::{self, UpdateProcessStreetSettingsRequest};
use crate::api::process_street_task_roles::{self, UpdateTaskRoleRequest};
use crate::api::test_support::empty_state;
use crate::api::AppState;

const KEY_ENV: &str = "INTEGRATION_SECRETS_ENCRYPTION_KEY";
const TEST_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const SECRET: &str = "TOPSECRET-do-not-log";

fn state() -> AppState {
    let _ = dotenvy::from_filename(".env.local");
    AppState {
        db: crate::db::connect_test(),
        ..empty_state()
    }
}

fn addr() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([203, 0, 113, 9], 0)))
}

fn admin(user: Uuid) -> crate::auth::AuthenticatedUser {
    caller(
        user,
        &["admin"],
        &["integrations.manage", "activity_logs.read"],
    )
}

/// The security-trail rows this actor wrote for `event`, newest first.
async fn auth_rows(superuser: &PgPool, actor: Uuid, event: &str) -> Vec<(Value, Option<String>)> {
    sqlx::query_as::<_, (Value, Option<String>)>(
        "SELECT metadata, host(ip_address) FROM auth.auth_audit_logs
          WHERE actor_user_id = $1 AND event_type = $2 ORDER BY created_at DESC",
    )
    .bind(actor)
    .bind(event)
    .fetch_all(superuser)
    .await
    .unwrap()
}

#[tokio::test]
#[serial_test::serial(ps_task_roles)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn audit_db_a_task_role_edit_is_recorded_with_actor_role_names_and_ip() {
    let state = state();
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "audit-task-role").await;

    let response = process_street_task_roles::update_task_role(
        State(state.clone()),
        admin(actor),
        addr(),
        HeaderMap::new(),
        Path("qms_credentials".to_string()),
        Json(UpdateTaskRoleRequest {
            task_names: vec![
                "Document Credentials".to_string(),
                "Add Credentials to QMS".to_string(),
            ],
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let rows = auth_rows(&superuser, actor, "integration_settings_updated").await;
    assert_eq!(rows.len(), 1);
    let (metadata, ip) = &rows[0];
    assert_eq!(metadata["integration"], "process_street_task_roles");
    assert_eq!(metadata["details"]["role"], "qms_credentials");
    assert_eq!(
        metadata["details"]["task_names"].as_array().unwrap().len(),
        2
    );
    assert_eq!(ip.as_deref(), Some("203.0.113.9"));
}

#[tokio::test]
#[serial_test::serial(ps_task_roles)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn audit_db_a_rejected_task_role_edit_writes_no_event() {
    let state = state();
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "audit-task-role-bad").await;

    let response = process_street_task_roles::update_task_role(
        State(state.clone()),
        admin(actor),
        addr(),
        HeaderMap::new(),
        Path("qms_credentials".to_string()),
        Json(UpdateTaskRoleRequest { task_names: vec![] }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    assert!(auth_rows(&superuser, actor, "integration_settings_updated")
        .await
        .is_empty());
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn audit_db_a_dropbox_settings_edit_is_recorded_without_any_secret() {
    std::env::set_var(KEY_ENV, TEST_KEY);
    let state = state();
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "audit-dropbox").await;

    let response = dropbox_settings::update_settings(
        State(state.clone()),
        admin(actor),
        addr(),
        HeaderMap::new(),
        Json(UpdateDropboxSettingsRequest {
            app_key: "app-key-1".to_string(),
            app_secret: SECRET.to_string(),
            refresh_token: SECRET.to_string(),
            root_namespace_id: "ns-1".to_string(),
            root_path: "/QMS Onboarding".to_string(),
        }),
    )
    .await;
    std::env::remove_var(KEY_ENV);
    assert_eq!(response.status(), StatusCode::OK);

    let rows = auth_rows(&superuser, actor, "integration_settings_updated").await;
    assert_eq!(rows.len(), 1);
    let metadata = &rows[0].0;
    assert_eq!(metadata["integration"], "dropbox");
    assert_eq!(metadata["details"]["app_secret_replaced"], true);
    assert_eq!(metadata["details"]["refresh_token_replaced"], true);
    assert_eq!(metadata["details"]["root_path"], "/QMS Onboarding");
    assert!(
        !metadata.to_string().contains(SECRET),
        "an audit row must never carry a secret value"
    );
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn audit_db_a_process_street_settings_edit_is_recorded_without_the_key() {
    std::env::set_var(KEY_ENV, TEST_KEY);
    let state = state();
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "audit-ps").await;

    let response = process_street_settings::update_settings(
        State(state.clone()),
        admin(actor),
        addr(),
        HeaderMap::new(),
        Json(UpdateProcessStreetSettingsRequest {
            schedule_mode: "daily_time".to_string(),
            sync_interval_hours: 24,
            sync_time: Some("02:30".to_string()),
            sync_timezone: Some("UTC".to_string()),
            api_key: SECRET.to_string(),
        }),
    )
    .await;
    std::env::remove_var(KEY_ENV);
    assert_eq!(response.status(), StatusCode::OK);

    let rows = auth_rows(&superuser, actor, "integration_settings_updated").await;
    assert_eq!(rows.len(), 1);
    let metadata = &rows[0].0;
    assert_eq!(metadata["integration"], "process_street");
    assert_eq!(metadata["details"]["api_key_replaced"], true);
    assert_eq!(metadata["details"]["schedule_mode"], "daily_time");
    assert_eq!(metadata["details"]["sync_time"], "02:30:00");
    assert_eq!(metadata["details"]["sync_timezone"], "UTC");
    assert!(
        !metadata.to_string().contains(SECRET),
        "an audit row must never carry the API key"
    );
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn audit_db_a_rejected_process_street_edit_writes_no_event() {
    std::env::set_var(KEY_ENV, TEST_KEY);
    let state = state();
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "audit-ps-bad").await;

    let response = process_street_settings::update_settings(
        State(state.clone()),
        admin(actor),
        addr(),
        HeaderMap::new(),
        Json(UpdateProcessStreetSettingsRequest {
            schedule_mode: "weekly".to_string(),
            sync_interval_hours: 24,
            sync_time: None,
            sync_timezone: None,
            api_key: SECRET.to_string(),
        }),
    )
    .await;
    std::env::remove_var(KEY_ENV);
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    assert!(auth_rows(&superuser, actor, "integration_settings_updated")
        .await
        .is_empty());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn audit_db_an_activity_log_export_is_itself_recorded() {
    let state = state();
    let superuser = superuser_pool();
    let actor = create_user(&superuser, "audit-export").await;

    let response = export_activity_logs(
        State(state.clone()),
        admin(actor),
        addr(),
        HeaderMap::new(),
        Json(ExportActivityLogsRequest {
            date_from: Utc::now() - Duration::days(1),
            date_to: Utc::now() + Duration::days(1),
            event_types: vec!["client_created".to_string()],
            entity_types: vec![],
            actor_user_ids: vec![],
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (metadata, entity_type, ip): (Value, String, Option<String>) = sqlx::query_as(
        "SELECT metadata, entity_type, host(ip_address) FROM client_ops.audit_log
          WHERE actor_user_id = $1 AND event_type = 'activity_log_exported'",
    )
    .bind(actor)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert_eq!(entity_type, "activity_log");
    assert_eq!(metadata["event_types"][0], "client_created");
    assert!(metadata["row_count"].is_number());
    assert_eq!(metadata["truncated"], false);
    assert_eq!(ip.as_deref(), Some("203.0.113.9"));
}
