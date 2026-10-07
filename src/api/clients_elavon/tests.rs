use axum::extract::{Json, Path, State};
use axum::http::{HeaderMap, StatusCode};
use uuid::Uuid;

use super::{
    get_facility_elavon, link_facility_elavon, resync_elavon_data, unlink_facility_elavon,
    LinkElavonRequest,
};
use crate::api::test_support::{empty_state, test_user};

#[tokio::test]
async fn get_facility_elavon_reaches_the_database() {
    let response = get_facility_elavon(
        State(empty_state()),
        test_user(),
        Path((Uuid::new_v4(), Uuid::new_v4())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn link_facility_elavon_refuses_insufficient_permission_without_touching_anything() {
    let response = link_facility_elavon(
        State(empty_state()),
        test_user(),
        HeaderMap::new(),
        Path((Uuid::new_v4(), Uuid::new_v4())),
        Json(LinkElavonRequest {
            merchant_account_run_id: "abc123".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn link_facility_elavon_rejects_a_blank_run_id() {
    let response = link_facility_elavon(
        State(empty_state()),
        crate::api::test_support::onboarding_manager_user(),
        HeaderMap::new(),
        Path((Uuid::new_v4(), Uuid::new_v4())),
        Json(LinkElavonRequest {
            merchant_account_run_id: "   ".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn unlink_facility_elavon_refuses_insufficient_permission_without_touching_anything() {
    let response = unlink_facility_elavon(
        State(empty_state()),
        test_user(),
        HeaderMap::new(),
        Path((Uuid::new_v4(), Uuid::new_v4())),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn unlink_facility_elavon_reaches_the_database() {
    let response = unlink_facility_elavon(
        State(empty_state()),
        crate::api::test_support::onboarding_manager_user(),
        HeaderMap::new(),
        Path((Uuid::new_v4(), Uuid::new_v4())),
    )
    .await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn resync_elavon_data_refuses_insufficient_permission_without_touching_anything() {
    let response = resync_elavon_data(
        State(empty_state()),
        test_user(),
        HeaderMap::new(),
        Path((Uuid::new_v4(), Uuid::new_v4())),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn resync_elavon_data_reports_not_configured_with_sufficient_permission() {
    let response = resync_elavon_data(
        State(empty_state()),
        crate::api::test_support::onboarding_manager_user(),
        HeaderMap::new(),
        Path((Uuid::new_v4(), Uuid::new_v4())),
    )
    .await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}
