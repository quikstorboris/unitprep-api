use axum::http::StatusCode;

use super::*;
use crate::api::discover::{discover, DiscoverRequest};
use crate::api::select_unit_file::{select_unit_file, SelectUnitFileRequest};
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

fn door_swap_document(file_name: &str) -> CsvDocument {
    CsvDocument {
        file_name: file_name.to_string(),
        headers: vec![
            "unit".to_string(),
            "status".to_string(),
            "unit type".to_string(),
            "customer".to_string(),
        ],
        rows: vec![vec![
            "1".to_string(),
            "rented".to_string(),
            "10x10 Non-Climate Controlled (10 x 10 x 8)".to_string(),
            "Lexie Rodrigue".to_string(),
        ]],
        modified_at: None,
    }
}

#[tokio::test]
async fn resolve_unit_format_returns_404_for_missing_session() {
    let response = resolve_unit_format(
        State(empty_state()),
        Json(ResolveUnitFormatRequest {
            session_id: "missing".to_string(),
            action: ResolveAction::Confirm,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resolve_unit_format_returns_409_before_discovery_completes() {
    let state = empty_state();
    state.unit_group_sessions.save(
        crate::application::unit_group_session::Session::new("s1".to_string()),
    );

    let response = resolve_unit_format(
        State(state),
        Json(ResolveUnitFormatRequest {
            session_id: "s1".to_string(),
            action: ResolveAction::Confirm,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn resolve_unit_format_rejects_when_no_file_is_selected_yet() {
    let state = uploaded_state(
        "s1",
        vec![qsx_document("a.csv"), qsx_document("b.csv")],
    );

    discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let response = resolve_unit_format(
        State(state),
        Json(ResolveUnitFormatRequest {
            session_id: "s1".to_string(),
            action: ResolveAction::Confirm,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["error"], "no_unit_file_selected");
}

#[tokio::test]
async fn confirm_applies_door_swaps_preset_mapping() {
    let state = uploaded_state("s1", vec![door_swap_document("Units List.csv")]);

    discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let response = resolve_unit_format(
        State(state),
        Json(ResolveUnitFormatRequest {
            session_id: "s1".to_string(),
            action: ResolveAction::Confirm,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["ready"], true);
    assert_eq!(
        body["discovered_group_names"],
        serde_json::json!(["10x10 Non-Climate Controlled (10 x 10 x 8)"])
    );
}

#[tokio::test]
async fn manual_map_rejects_when_a_required_field_is_left_unmapped() {
    let state = uploaded_state("s1", vec![door_swap_document("Units List.csv")]);

    discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let response = resolve_unit_format(
        State(state),
        Json(ResolveUnitFormatRequest {
            session_id: "s1".to_string(),
            action: ResolveAction::Map {
                mapping: vec![MappingEntryInput {
                    target: "Number".to_string(),
                    source: Some("unit".to_string()),
                }],
            },
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["error"], "mapping_incomplete");
}

#[tokio::test]
async fn manual_map_rejects_a_source_header_not_in_the_file() {
    let state = uploaded_state("s1", vec![door_swap_document("Units List.csv")]);

    discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let response = resolve_unit_format(
        State(state),
        Json(ResolveUnitFormatRequest {
            session_id: "s1".to_string(),
            action: ResolveAction::Map {
                mapping: vec![
                    MappingEntryInput {
                        target: "Number".to_string(),
                        source: Some("does_not_exist".to_string()),
                    },
                    MappingEntryInput {
                        target: "UnitGroup".to_string(),
                        source: Some("unit type".to_string()),
                    },
                ],
            },
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["error"], "unknown_source_header");
}

#[tokio::test]
async fn manual_map_succeeds_with_only_the_required_fields_mapped() {
    let state = uploaded_state("s1", vec![door_swap_document("Units List.csv")]);

    discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    let response = resolve_unit_format(
        State(state),
        Json(ResolveUnitFormatRequest {
            session_id: "s1".to_string(),
            action: ResolveAction::Map {
                mapping: vec![
                    MappingEntryInput {
                        target: "Number".to_string(),
                        source: Some("unit".to_string()),
                    },
                    MappingEntryInput {
                        target: "UnitGroup".to_string(),
                        source: Some("unit type".to_string()),
                    },
                ],
            },
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["ready"], true);
    assert_eq!(
        body["discovered_group_names"],
        serde_json::json!(["10x10 Non-Climate Controlled (10 x 10 x 8)"])
    );
}

/// Also exercises `/unit-file/select` immediately before resolving, so
/// the two new endpoints are proven to compose the way the frontend will
/// actually call them: select, then resolve.
#[tokio::test]
async fn select_then_confirm_across_two_candidates() {
    let state = uploaded_state(
        "s1",
        vec![qsx_document("a.csv"), qsx_document("b.csv")],
    );

    discover(
        State(state.clone()),
        Json(DiscoverRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    select_unit_file(
        State(state.clone()),
        Json(SelectUnitFileRequest {
            session_id: "s1".to_string(),
            unit_file_name: "b.csv".to_string(),
        }),
    )
    .await;

    let response = resolve_unit_format(
        State(state),
        Json(ResolveUnitFormatRequest {
            session_id: "s1".to_string(),
            action: ResolveAction::Confirm,
        }),
    )
    .await;

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["selected_unit_file_name"], "b.csv");
    assert_eq!(body["ready"], true);
}
