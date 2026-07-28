use axum::http::StatusCode;

use super::*;
use crate::api::discover::{discover, DiscoverRequest};
use crate::api::group_file_upload::apply_group_file_upload;
use crate::api::resolve_unit_format::{
    resolve_unit_format, ResolveAction, ResolveUnitFormatRequest,
};
use crate::api::test_support::{discovered_state, empty_state};
use unitprep_core::csv_document::CsvDocument;

fn document(file_name: &str, headers: Vec<&str>) -> CsvDocument {
    CsvDocument {
        file_name: file_name.to_string(),
        headers: headers.into_iter().map(|h| h.to_string()).collect(),
        rows: Vec::new(),
        modified_at: None,
    }
}

fn qsx_document(file_name: &str) -> CsvDocument {
    document(file_name, vec!["number", "unitgroup", "category"])
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn returns_404_for_missing_session() {
    let response = confirm_group_file(
        State(empty_state()),
        Json(ConfirmGroupFileRequest {
            session_id: "missing".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn returns_409_before_discovery_completes() {
    let state = empty_state();

    state
        .unit_group_sessions
        .save(crate::application::unit_group_session::Session::new(
            "s1".to_string(),
        ));

    let response = confirm_group_file(
        State(state),
        Json(ConfirmGroupFileRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn rejects_confirming_when_nothing_is_selected() {
    let state = discovered_state("s1", vec![qsx_document("units.csv")]);

    let response = confirm_group_file(
        State(state),
        Json(ConfirmGroupFileRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = body_json(response).await;

    assert_eq!(body["error"], "no_group_file_selected");
}

#[tokio::test]
async fn rejects_confirming_an_invalid_format_file() {
    let state = discovered_state("s1", vec![qsx_document("units.csv")]);

    apply_group_file_upload(
        &state,
        "s1",
        document("master_groups.csv", vec!["Name", "Description"]),
    );

    let response = confirm_group_file(
        State(state),
        Json(ConfirmGroupFileRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = body_json(response).await;

    assert_eq!(body["error"], "group_file_format_invalid");
}

#[tokio::test]
async fn confirms_a_valid_selected_file() {
    let state = discovered_state("s1", vec![qsx_document("units.csv")]);

    apply_group_file_upload(
        &state,
        "s1",
        document("master_groups.csv", vec!["Name", "Description", "Active"]),
    );

    let response = confirm_group_file(
        State(state),
        Json(ConfirmGroupFileRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;

    assert_eq!(body["group_file_confirmed"], true);
}

/// The full picture: unit file resolved + group file selected, valid,
/// and explicitly confirmed is what actually makes discovery `ready` --
/// selecting/validating alone isn't enough.
#[tokio::test]
async fn confirming_the_group_file_is_what_makes_discovery_ready() {
    let state = discovered_state("s1", vec![qsx_document("units.csv")]);

    discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    resolve_unit_format(
        State(state.clone()),
        Json(ResolveUnitFormatRequest {
            session_id: "s1".to_string(),
            action: ResolveAction::Confirm,
        }),
    )
    .await;

    apply_group_file_upload(
        &state,
        "s1",
        document("master_groups.csv", vec!["Name", "Description", "Active"]),
    );

    let before_confirm = discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let body = body_json(before_confirm).await;
    assert_eq!(body["ready"], false, "selected+valid but not yet confirmed");

    let response = confirm_group_file(
        State(state),
        Json(ConfirmGroupFileRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let body = body_json(response).await;

    assert_eq!(body["ready"], true);
}
