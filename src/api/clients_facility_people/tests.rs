use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
};
use uuid::Uuid;

use super::add::*;
use super::edit::*;
use super::get::*;
use super::unlink::*;
use crate::api::test_support::{empty_state, test_user};

#[tokio::test]
async fn get_facility_people_reaches_the_database() {
    let response = get_facility_people(
        State(empty_state()),
        test_user(),
        Path((Uuid::new_v4(), Uuid::new_v4())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn add_facility_person_rejects_a_blank_full_name_without_touching_the_database() {
    let response = add_facility_person(
        State(empty_state()),
        test_user(),
        HeaderMap::new(),
        Path((Uuid::new_v4(), Uuid::new_v4())),
        Json(AddPersonRequest {
            full_name: "   ".to_string(),
            email: Some("someone@example.com".to_string()),
            phone: None,
            role: "owner".to_string(),
            source: "process_street".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn add_facility_person_rejects_an_unrecognized_source_without_touching_the_database() {
    let response = add_facility_person(
        State(empty_state()),
        test_user(),
        HeaderMap::new(),
        Path((Uuid::new_v4(), Uuid::new_v4())),
        Json(AddPersonRequest {
            full_name: "Irene Chen".to_string(),
            email: Some("irene@chenlawgroup.com".to_string()),
            phone: None,
            role: "owner".to_string(),
            source: "not_a_real_source".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn add_facility_person_reaches_the_database() {
    let response = add_facility_person(
        State(empty_state()),
        test_user(),
        HeaderMap::new(),
        Path((Uuid::new_v4(), Uuid::new_v4())),
        Json(AddPersonRequest {
            full_name: "Irene Chen".to_string(),
            email: Some("irene@chenlawgroup.com".to_string()),
            phone: Some("(301) 787-9221".to_string()),
            role: "owner".to_string(),
            source: "manual".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn edit_facility_person_rejects_a_blank_full_name_without_touching_the_database() {
    let response = edit_facility_person(
        State(empty_state()),
        test_user(),
        HeaderMap::new(),
        Path((Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())),
        Json(EditPersonRequest {
            old_role: "owner".to_string(),
            full_name: "   ".to_string(),
            email: None,
            phone: None,
            role: "owner".to_string(),
            protect_from_resync: false,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn edit_facility_person_reaches_the_database() {
    let response = edit_facility_person(
        State(empty_state()),
        test_user(),
        HeaderMap::new(),
        Path((Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())),
        Json(EditPersonRequest {
            old_role: "owner".to_string(),
            full_name: "Irene Chen".to_string(),
            email: Some("irene@chenlawgroup.com".to_string()),
            phone: None,
            role: "owner".to_string(),
            protect_from_resync: true,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn unlink_facility_person_reaches_the_database() {
    let response = unlink_facility_person(
        State(empty_state()),
        test_user(),
        HeaderMap::new(),
        Path((Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4())),
        axum::extract::Query(UnlinkFacilityPersonQuery {
            role: "owner".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
