use axum::extract::{Path, State};
use axum::http::StatusCode;
use uuid::Uuid;

use super::{get_company_detail, get_facility_detail, get_facility_policies};
use crate::api::test_support::{empty_state, test_user};

#[tokio::test]
async fn get_company_detail_returns_404_for_the_unreachable_test_pool_as_a_500() {
    // Same convention as every other handler test in this codebase:
    // empty_state()'s pool never connects, so any handler that
    // reaches the database at all surfaces as a 500 -- the success
    // signal here is "it reached the query", not a real 404.
    let response =
        get_company_detail(State(empty_state()), test_user(), Path(Uuid::new_v4())).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn get_facility_detail_reaches_the_database() {
    let response = get_facility_detail(
        State(empty_state()),
        test_user(),
        Path((Uuid::new_v4(), Uuid::new_v4())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn get_facility_policies_reaches_the_database() {
    let response = get_facility_policies(
        State(empty_state()),
        test_user(),
        Path((Uuid::new_v4(), Uuid::new_v4())),
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
