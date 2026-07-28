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

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn select_unit_file_returns_404_for_missing_session() {
    let response = select_unit_file(
        State(empty_state()),
        Json(SelectUnitFileRequest {
            session_id: "missing".to_string(),
            unit_file_names: vec!["units.csv".to_string()],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn select_unit_file_returns_409_before_discovery_completes() {
    let state = empty_state();
    state
        .unit_group_sessions
        .save(crate::application::unit_group_session::Session::new(
            "s1".to_string(),
        ));

    let response = select_unit_file(
        State(state),
        Json(SelectUnitFileRequest {
            session_id: "s1".to_string(),
            unit_file_names: vec!["units.csv".to_string()],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn select_unit_file_rejects_an_empty_selection() {
    let state = uploaded_state("s1", vec![qsx_document("units.csv", None)]);

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
            unit_file_names: Vec::new(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = body_json(response).await;

    assert_eq!(body["error"], "unit_file_selection_empty");
}

#[tokio::test]
async fn select_unit_file_rejects_a_file_discovery_never_found() {
    let state = uploaded_state("s1", vec![qsx_document("units.csv", None)]);

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
            unit_file_names: vec!["not_discovered.csv".to_string()],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = body_json(response).await;

    assert_eq!(body["error"], "unit_file_invalid");
}

/// A folder can hold several genuinely distinct facilities' unit files at
/// once (not just duplicate dated re-pulls of one facility) — discovery
/// must let the user confirm which of them to process, defaulting to all,
/// not force a single winner.
#[tokio::test]
async fn selecting_a_subset_of_candidates_becomes_the_confirmed_set() {
    let state = uploaded_state(
        "s1",
        vec![
            qsx_document("facility_a.csv", Some(1_000)),
            qsx_document("facility_b.csv", Some(2_000)),
            qsx_document("facility_c.csv", Some(3_000)),
        ],
    );

    let discover_response = discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let body = body_json(discover_response).await;

    assert_eq!(body["requires_unit_file_selection"], true);
    assert_eq!(body["selected_unit_file_names"], serde_json::json!([]));

    let response = select_unit_file(
        State(state),
        Json(SelectUnitFileRequest {
            session_id: "s1".to_string(),
            unit_file_names: vec!["facility_a.csv".to_string(), "facility_c.csv".to_string()],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;

    assert_eq!(body["requires_unit_file_selection"], false);

    assert_eq!(
        body["selected_unit_file_names"],
        serde_json::json!(["facility_a.csv", "facility_c.csv"])
    );

    // facility_b.csv was left out of the confirmed set entirely.
    assert_eq!(body["current_unit_file_name"], "facility_a.csv");

    assert_eq!(
        body["pending_unit_file_names"],
        serde_json::json!(["facility_a.csv", "facility_c.csv"])
    );

    // Selected but not yet confirmed/mapped.
    assert_eq!(body["requires_format_resolution"], true);
}

/// The "select all" master checkbox case — confirming every candidate at
/// once.
#[tokio::test]
async fn selecting_every_candidate_confirms_them_all() {
    let state = uploaded_state(
        "s1",
        vec![
            qsx_document("facility_a.csv", Some(1_000)),
            qsx_document("facility_b.csv", Some(2_000)),
        ],
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
            unit_file_names: vec!["facility_a.csv".to_string(), "facility_b.csv".to_string()],
        }),
    )
    .await;

    let body = body_json(response).await;

    assert_eq!(
        body["selected_unit_file_names"],
        serde_json::json!(["facility_a.csv", "facility_b.csv"])
    );
}
