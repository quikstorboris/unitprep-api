use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Json, State};
use axum::http::{HeaderMap, StatusCode};
use uuid::Uuid;

use super::*;
use crate::api::test_support::{clickup_user, empty_state, test_user};
use crate::integrations::secrets;

fn local_addr() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
}

#[tokio::test]
async fn get_refuses_a_caller_without_the_permission() {
    let response = get_connection(State(empty_state()), test_user()).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn save_refuses_a_caller_without_the_permission() {
    let response = save_token(
        State(empty_state()),
        test_user(),
        local_addr(),
        HeaderMap::new(),
        Json(SaveTokenRequest {
            token: "pk_whatever".to_string(),
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_refuses_a_caller_without_the_permission() {
    let response = test_connection(
        State(empty_state()),
        test_user(),
        local_addr(),
        HeaderMap::new(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn remove_refuses_a_caller_without_the_permission() {
    let response = remove_token(
        State(empty_state()),
        test_user(),
        local_addr(),
        HeaderMap::new(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn get_with_the_permission_gets_past_the_gate_and_reaches_the_database() {
    // The disconnected test pool then fails: 500, not 403.
    let response = get_connection(State(empty_state()), clickup_user()).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn save_rejects_an_empty_token_before_calling_clickup() {
    let response = save_token(
        State(empty_state()),
        clickup_user(),
        local_addr(),
        HeaderMap::new(),
        Json(SaveTokenRequest {
            token: "   ".to_string(),
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn save_rejects_a_token_containing_whitespace() {
    let response = save_token(
        State(empty_state()),
        clickup_user(),
        local_addr(),
        HeaderMap::new(),
        Json(SaveTokenRequest {
            token: "pk_abc def".to_string(),
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn save_rejects_an_oversized_token() {
    let response = save_token(
        State(empty_state()),
        clickup_user(),
        local_addr(),
        HeaderMap::new(),
        Json(SaveTokenRequest {
            token: "x".repeat(MAX_TOKEN_LEN + 1),
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[test]
fn the_aad_is_bound_to_the_owning_user() {
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    assert_ne!(aad(a), aad(b));
    assert_eq!(aad(a), aad(a));
}

#[test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
fn a_ciphertext_does_not_decrypt_under_another_users_aad() {
    std::env::set_var(
        "INTEGRATION_SECRETS_ENCRYPTION_KEY",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );

    let owner = Uuid::new_v4();
    let someone_else = Uuid::new_v4();
    let blob = secrets::encrypt(&aad(owner), "pk_secret").unwrap();

    assert_eq!(secrets::decrypt(&aad(owner), &blob).unwrap(), "pk_secret");
    assert!(secrets::decrypt(&aad(someone_else), &blob).is_err());

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}
