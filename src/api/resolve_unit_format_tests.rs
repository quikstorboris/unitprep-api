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
    state
        .unit_group_sessions
        .save(crate::application::unit_group_session::Session::new(
            "s1".to_string(),
        ));

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
    let state = uploaded_state("s1", vec![qsx_document("a.csv"), qsx_document("b.csv")]);

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

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

/// Also exercises `/unit-file/select` immediately before resolving, so
/// the two endpoints are proven to compose the way the frontend will
/// actually call them: select, then resolve.
#[tokio::test]
async fn select_then_confirm_a_single_candidate() {
    let state = uploaded_state("s1", vec![qsx_document("a.csv"), qsx_document("b.csv")]);

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
            unit_file_names: vec!["b.csv".to_string()],
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

    let body = body_json(response).await;

    assert_eq!(
        body["selected_unit_file_names"],
        serde_json::json!(["b.csv"])
    );
    assert_eq!(body["ready"], true);
}

/// The core new capability: confirming one file's vendor now resolves
/// every confirmed file sharing that exact header shape in the same
/// action, instead of requiring a click per file.
#[tokio::test]
async fn confirm_bulk_resolves_every_matching_selected_file() {
    let state = uploaded_state(
        "s1",
        vec![
            qsx_document("a.csv"),
            qsx_document("b.csv"),
            qsx_document("c.csv"),
        ],
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
            unit_file_names: vec![
                "a.csv".to_string(),
                "b.csv".to_string(),
                "c.csv".to_string(),
            ],
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

    let body = body_json(response).await;

    assert_eq!(body["current_unit_file_name"], serde_json::Value::Null);
    assert_eq!(body["requires_format_resolution"], false);
    assert_eq!(body["ready"], true);
    assert_eq!(
        body["selected_unit_file_names"],
        serde_json::json!(["a.csv", "b.csv", "c.csv"])
    );
}

/// The safeguard: a confirmed set spanning more than one header shape
/// must not be bulk-confirmed together -- surfaced as a clear error
/// naming the outlier file(s), not a silent partial resolution.
#[tokio::test]
async fn confirm_rejects_a_confirmed_set_with_mismatched_headers() {
    let state = uploaded_state(
        "s1",
        vec![
            qsx_document("a.csv"),
            qsx_document("b.csv"),
            door_swap_document("odd_one_out.csv"),
        ],
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
            unit_file_names: vec![
                "a.csv".to_string(),
                "b.csv".to_string(),
                "odd_one_out.csv".to_string(),
            ],
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

    assert_eq!(response.status(), StatusCode::CONFLICT);

    let body = body_json(response).await;

    assert_eq!(body["error"], "unit_file_header_mismatch");
    assert!(body["message"]
        .as_str()
        .unwrap()
        .contains("odd_one_out.csv"));
}

/// The frontend's "Change Vendor" button: once every selected file is
/// resolved, Reset undoes that -- the confirm/map screen reappears for
/// the whole selected set, exactly the state the user was in right
/// before they confirmed.
#[tokio::test]
async fn reset_reopens_format_resolution_after_everything_was_confirmed() {
    let state = uploaded_state("s1", vec![qsx_document("a.csv"), qsx_document("b.csv")]);

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
            unit_file_names: vec!["a.csv".to_string(), "b.csv".to_string()],
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

    let response = resolve_unit_format(
        State(state),
        Json(ResolveUnitFormatRequest {
            session_id: "s1".to_string(),
            action: ResolveAction::Reset,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;

    assert_eq!(body["requires_format_resolution"], true);
    assert_eq!(body["current_unit_file_name"], "a.csv");
    assert_eq!(
        body["pending_unit_file_names"],
        serde_json::json!(["a.csv", "b.csv"])
    );
}

/// Reset must not require a "current file to resolve" to already exist
/// -- calling it right after Confirm (its actual use case) means there
/// isn't one, since everything's already resolved at that point.
#[tokio::test]
async fn reset_succeeds_even_though_nothing_is_currently_pending() {
    let state = uploaded_state("s1", vec![qsx_document("a.csv")]);

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

    let response = resolve_unit_format(
        State(state),
        Json(ResolveUnitFormatRequest {
            session_id: "s1".to_string(),
            action: ResolveAction::Reset,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
}

/// Manual mapping remains scoped to a single file even when other files
/// are still selected and pending -- it's a per-file escape hatch, not a
/// bulk action like Confirm.
#[tokio::test]
async fn manual_map_resolves_only_the_current_file() {
    let state = uploaded_state(
        "s1",
        vec![
            door_swap_document("first.csv"),
            door_swap_document("second.csv"),
        ],
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
            unit_file_names: vec!["first.csv".to_string(), "second.csv".to_string()],
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

    let body = body_json(response).await;

    assert_eq!(body["current_unit_file_name"], "second.csv");
    assert_eq!(body["requires_format_resolution"], true);
}
