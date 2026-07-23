use axum::http::StatusCode;

use super::*;
use crate::api::discover::{discover, DiscoverRequest};
use crate::api::test_support::{empty_state, uploaded_state};
use unitprep_core::csv_document::CsvDocument;

fn qsx_document(file_name: &str, modified_at: Option<i64>) -> CsvDocument {
    CsvDocument {
        file_name: file_name.to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "category".to_string(),
        ],
        rows: Vec::new(),
        modified_at,
    }
}

#[tokio::test]
async fn select_unit_file_returns_404_for_missing_session() {
    let response = select_unit_file(
        State(empty_state()),
        Json(SelectUnitFileRequest {
            session_id: "missing".to_string(),
            unit_file_name: "units.csv".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn select_unit_file_returns_409_before_discovery_completes() {
    let state = empty_state();
    state.unit_group_sessions.save(
        crate::application::unit_group_session::Session::new("s1".to_string()),
    );

    let response = select_unit_file(
        State(state),
        Json(SelectUnitFileRequest {
            session_id: "s1".to_string(),
            unit_file_name: "units.csv".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn select_unit_file_rejects_a_file_discovery_never_found() {
    let state = uploaded_state(
        "s1",
        vec![qsx_document("units.csv", None)],
    );

    discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let response = select_unit_file(
        State(state),
        Json(SelectUnitFileRequest {
            session_id: "s1".to_string(),
            unit_file_name: "not_discovered.csv".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["error"], "unit_file_invalid");
}

/// Two candidate unit files (e.g. re-pulls on different dates) force a
/// selection — this is the scenario `/unit-file/select` exists for.
#[tokio::test]
async fn selecting_among_multiple_candidates_makes_it_the_selected_file() {
    let state = uploaded_state(
        "s1",
        vec![
            qsx_document("units_july1.csv", Some(1_000)),
            qsx_document("units_july15.csv", Some(2_000)),
        ],
    );

    let discover_response = discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let bytes = axum::body::to_bytes(discover_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["requires_unit_file_selection"], true);
    assert_eq!(body["selected_unit_file_name"], serde_json::Value::Null);

    let response = select_unit_file(
        State(state),
        Json(SelectUnitFileRequest {
            session_id: "s1".to_string(),
            unit_file_name: "units_july15.csv".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["requires_unit_file_selection"], false);
    assert_eq!(body["selected_unit_file_name"], "units_july15.csv");
    // Selected but not yet confirmed/mapped.
    assert_eq!(body["requires_format_resolution"], true);
}
