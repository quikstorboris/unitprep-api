use axum::http::StatusCode;

use super::*;
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
    let response = apply_group_file_upload(
        &empty_state(),
        "missing",
        document("groups.csv", vec!["Name"]),
    );

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn returns_409_before_discovery_completes() {
    let state = empty_state();

    state
        .unit_group_sessions
        .save(crate::application::unit_group_session::Session::new(
            "s1".to_string(),
            None,
        ));

    let response = apply_group_file_upload(&state, "s1", document("groups.csv", vec!["Name"]));

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

/// The core scenario the feature exists for: discovery found zero
/// master group files, but one actually exists in the folder under a
/// name/shape discovery's own heuristic didn't recognize -- the user
/// manually uploads it and it becomes the selected master file. Its
/// headers here don't satisfy the real group-file format (missing
/// assignedto/status/lastupdated), so it's flagged invalid -- forcing an
/// override doesn't waive the format check, just the auto-classification.
#[tokio::test]
async fn manually_uploaded_file_becomes_the_selected_group_file() {
    let state = discovered_state("s1", vec![qsx_document("units.csv")]);

    let response = apply_group_file_upload(
        &state,
        "s1",
        document("master_groups.csv", vec!["Name", "Description"]),
    );

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;

    assert_eq!(body["selected_group_file_name"], "master_groups.csv");
    assert_eq!(body["group_file_format_valid"], false);
}

/// A manually-uploaded file that does have every required header is
/// flagged valid, and factors into `ready` like an auto-detected one
/// would.
#[tokio::test]
async fn manually_uploaded_file_with_conforming_headers_is_valid() {
    let state = discovered_state("s1", vec![qsx_document("units.csv")]);

    let response = apply_group_file_upload(
        &state,
        "s1",
        document(
            "master_groups.csv",
            vec!["Name", "Description", "AssignedTo", "Status", "LastUpdated"],
        ),
    );

    let body = body_json(response).await;

    assert_eq!(body["group_file_format_valid"], true);
}

/// The minimal header set (Name, Description, Active) is just as valid
/// as the full one -- either is a real master group file.
#[tokio::test]
async fn manually_uploaded_file_with_minimal_headers_is_valid() {
    let state = discovered_state("s1", vec![qsx_document("units.csv")]);

    let response = apply_group_file_upload(
        &state,
        "s1",
        document("master_groups.csv", vec!["Name", "Description", "Active"]),
    );

    let body = body_json(response).await;

    assert_eq!(body["group_file_format_valid"], true);
}

/// Uploading a *different* file resets any previous confirmation --
/// "Select Different File" must not silently carry the old confirmation
/// forward onto a file the user hasn't actually looked at yet.
#[tokio::test]
async fn reuploading_a_different_file_resets_confirmation() {
    let state = discovered_state("s1", vec![qsx_document("units.csv")]);

    apply_group_file_upload(
        &state,
        "s1",
        document("master_groups.csv", vec!["Name", "Description", "Active"]),
    );

    crate::api::group_file_confirm::confirm_group_file(
        axum::extract::State(state.clone()),
        crate::api::test_support::test_user(),
        axum::Json(crate::api::group_file_confirm::ConfirmGroupFileRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let response = apply_group_file_upload(
        &state,
        "s1",
        document(
            "different_groups.csv",
            vec!["Name", "Description", "Active"],
        ),
    );

    let body = body_json(response).await;

    assert_eq!(body["selected_group_file_name"], "different_groups.csv");
    assert_eq!(body["group_file_confirmed"], false);
}

/// A second call to /discover after a manual override must not drop the
/// forced selection just because the manually chosen file doesn't
/// independently classify as a group document.
#[tokio::test]
async fn manual_selection_survives_a_subsequent_discovery_recompute() {
    let state = discovered_state("s1", vec![qsx_document("units.csv")]);

    apply_group_file_upload(
        &state,
        "s1",
        document("master_groups.csv", vec!["Name", "Description"]),
    );

    let recomputed = crate::api::discover::discover(
        axum::extract::State(state),
        crate::api::test_support::test_user(),
        axum::Json(crate::api::discover::DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let body = body_json(recomputed).await;

    assert_eq!(body["selected_group_file_name"], "master_groups.csv");
}
