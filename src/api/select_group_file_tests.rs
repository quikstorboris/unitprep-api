use axum::http::StatusCode;

use super::*;
use crate::api::discover::{discover, DiscoverRequest};
use crate::api::test_support::{empty_state, uploaded_state};
use unitprep_core::csv_document::CsvDocument;

fn qsx_document(file_name: &str) -> CsvDocument {
    CsvDocument {
        file_name: file_name.to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "category".to_string(),
        ],
        rows: Vec::new(),
        modified_at: None,
    }
}

fn group_document(file_name: &str) -> CsvDocument {
    CsvDocument {
        file_name: file_name.to_string(),
        headers: vec![
            "name".to_string(),
            "description".to_string(),
            "active".to_string(),
        ],
        rows: Vec::new(),
        modified_at: None,
    }
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn returns_404_for_missing_session() {
    let response = select_group_file(
        State(empty_state()),
        Json(SelectGroupFileRequest {
            session_id: "missing".to_string(),
            group_file_name: "a.csv".to_string(),
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

    let response = select_group_file(
        State(state),
        Json(SelectGroupFileRequest {
            session_id: "s1".to_string(),
            group_file_name: "a.csv".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

/// The core scenario this endpoint exists for: discovery found more than
/// one candidate (so nothing auto-selected), and the user picks one from
/// the list explicitly.
#[tokio::test]
async fn selects_one_of_several_auto_discovered_candidates() {
    let state = uploaded_state(
        "s1",
        vec![
            qsx_document("units.csv"),
            group_document("wave_root_groups.csv"),
            group_document("facility_a_groups.csv"),
        ],
    );

    discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let response = select_group_file(
        State(state),
        Json(SelectGroupFileRequest {
            session_id: "s1".to_string(),
            group_file_name: "wave_root_groups.csv".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;

    assert_eq!(body["selected_group_file_name"], "wave_root_groups.csv");
    assert_eq!(body["group_file_confirmed"], false);
}

/// Picking a name that wasn't one of the actual candidates (stale
/// frontend state, tampered request) must be rejected, not silently
/// accepted as a forced override -- that's what `/group-file/upload`
/// exists for.
#[tokio::test]
async fn rejects_a_name_that_is_not_an_actual_candidate() {
    let state = uploaded_state(
        "s1",
        vec![
            qsx_document("units.csv"),
            group_document("wave_root_groups.csv"),
            group_document("facility_a_groups.csv"),
        ],
    );

    discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let response = select_group_file(
        State(state),
        Json(SelectGroupFileRequest {
            session_id: "s1".to_string(),
            group_file_name: "not_a_real_candidate.csv".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = body_json(response).await;

    assert_eq!(body["error"], "unknown_group_file_candidate");
}

/// A selection made among ambiguous candidates must survive a
/// subsequent `/discover` recompute the same way a manual upload's
/// forced selection does (see
/// `manual_selection_survives_a_subsequent_discovery_recompute` in
/// `group_file_upload_tests.rs`) -- otherwise the user's pick would
/// silently revert every time the page re-fetches discovery state.
#[tokio::test]
async fn selection_survives_a_subsequent_discovery_recompute() {
    let state = uploaded_state(
        "s1",
        vec![
            qsx_document("units.csv"),
            group_document("wave_root_groups.csv"),
            group_document("facility_a_groups.csv"),
        ],
    );

    discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    select_group_file(
        State(state.clone()),
        Json(SelectGroupFileRequest {
            session_id: "s1".to_string(),
            group_file_name: "facility_a_groups.csv".to_string(),
        }),
    )
    .await;

    let recomputed = discover(
        State(state),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let body = body_json(recomputed).await;

    assert_eq!(body["selected_group_file_name"], "facility_a_groups.csv");
}
