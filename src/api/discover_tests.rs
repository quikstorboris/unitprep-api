use axum::http::StatusCode;

use super::*;
use crate::api::resolve_unit_format::{resolve_unit_format, ResolveAction, ResolveUnitFormatRequest};
use crate::api::test_support::{
    empty_state,
    uploaded_state,
};
use unitprep_core::csv_document::CsvDocument;

#[tokio::test]
async fn discover_returns_404_for_missing_session(
) {
    let response = discover(
        State(empty_state()),
        Json(DiscoverRequest {
            session_id: "missing"
                .to_string(),
        }),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND
    );
}

/// A file matching a known vendor's signature is a *candidate*, not
/// immediately usable — every vendor (QSX included) needs an explicit
/// confirm-or-map step before discovery can be `ready`. This test covers
/// the "just discovered, nothing confirmed yet" half of that; the
/// following test confirms the format and checks the rest.
#[tokio::test]
async fn discover_classifies_unit_and_group_files_but_is_not_ready_until_format_resolved(
) {
    let unit_doc = CsvDocument {
        modified_at: None,
        file_name: "units.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "category".to_string(),
        ],
        rows: Vec::new(),
    };

    let group_doc = CsvDocument {
        modified_at: None,
        file_name: "groups.csv"
            .to_string(),
        headers: vec![
            "name".to_string(),
            "description".to_string(),
            "assignedto".to_string(),
            "status".to_string(),
            "lastupdated".to_string(),
        ],
        rows: Vec::new(),
    };

    let state = uploaded_state(
        "s1",
        vec![unit_doc, group_doc],
    );

    let response = discover(
        State(state),
        Json(DiscoverRequest {
            session_id: "s1"
                .to_string(),
        }),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::OK
    );

    let bytes = axum::body::to_bytes(
        response.into_body(),
        usize::MAX,
    )
    .await
    .unwrap();

    let body: serde_json::Value =
        serde_json::from_slice(
            &bytes,
        )
        .unwrap();

    assert_eq!(
        body["unit_files_found"],
        1
    );

    assert_eq!(
        body["group_files_found"],
        1
    );

    assert_eq!(
        body["requires_unit_file_selection"],
        false
    );

    assert_eq!(
        body["requires_format_resolution"],
        true
    );

    assert_eq!(
        body["detected_vendor_name"],
        "QSX"
    );

    assert_eq!(
        body["ready"], false
    );
}

/// Confirming the detected vendor is what actually makes discovery ready
/// — this exercises the full discover -> resolve-format flow.
#[tokio::test]
async fn confirming_the_detected_vendor_makes_discovery_ready(
) {
    let unit_doc = CsvDocument {
        modified_at: None,
        file_name: "units.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "category".to_string(),
        ],
        rows: Vec::new(),
    };

    let state = uploaded_state(
        "s1",
        vec![unit_doc],
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

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["requires_format_resolution"], false);
    assert_eq!(body["ready"], true);
}

/// A net-new client has nothing in QMS yet, so there's no master group
/// file to discover at all — once the unit file's format is confirmed,
/// this must be `ready`, not stuck waiting for a selection that has no
/// candidates to select from.
#[tokio::test]
async fn discover_is_ready_with_zero_group_files_once_format_is_confirmed(
) {
    let unit_doc = CsvDocument {
        modified_at: None,
        file_name: "units.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "category".to_string(),
        ],
        rows: Vec::new(),
    };

    let state = uploaded_state(
        "s1",
        vec![unit_doc],
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

    let bytes = axum::body::to_bytes(
        response.into_body(),
        usize::MAX,
    )
    .await
    .unwrap();

    let body: serde_json::Value =
        serde_json::from_slice(
            &bytes,
        )
        .unwrap();

    assert_eq!(
        body["group_files_found"],
        0
    );

    assert_eq!(
        body["requires_group_selection"],
        false
    );

    assert_eq!(
        body["selected_group_file_name"],
        serde_json::Value::Null
    );

    assert_eq!(
        body["ready"], true
    );
}

/// The group names shown alongside "no master file" only matter once
/// there's real row data to extract them from *and* the format has been
/// confirmed — distinct from the zero-group-file readiness test above,
/// which uses an empty unit file.
#[tokio::test]
async fn discover_lists_distinct_group_names_from_unit_files_once_format_is_confirmed(
) {
    let unit_doc = CsvDocument {
        modified_at: None,
        file_name: "units.csv"
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "category".to_string(),
        ],
        rows: vec![
            vec![
                "A01".to_string(),
                "10x10 Climate".to_string(),
                "Standard".to_string(),
            ],
            vec![
                "A02".to_string(),
                "10x10 Climate".to_string(),
                "Standard".to_string(),
            ],
            vec![
                "A03".to_string(),
                "10x20 Outside".to_string(),
                "Standard".to_string(),
            ],
        ],
    };

    let state = uploaded_state(
        "s1",
        vec![unit_doc],
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

    let bytes = axum::body::to_bytes(
        response.into_body(),
        usize::MAX,
    )
    .await
    .unwrap();

    let body: serde_json::Value =
        serde_json::from_slice(
            &bytes,
        )
        .unwrap();

    assert_eq!(
        body["discovered_group_names"],
        serde_json::json!([
            "10x10 Climate",
            "10x20 Outside"
        ])
    );
}

/// Regression test for the exact bug this fix closes: a unit file
/// whose headers use underscores/spaces (e.g. "Unit_Group" instead
/// of "UnitGroup") must still be classified as a unit file — and,
/// critically, validation downstream must still be able to find
/// those same columns (see the equivalent `header_index` tests in
/// `unitprep-core`'s csv_document tests and
/// `unitprep-unit-group`'s
/// `validate_document_errors_loudly_when_a_supposed_unit_file_has_no_matching_columns`).
#[tokio::test]
async fn discover_classifies_unit_file_with_underscored_headers(
) {
    let unit_doc = CsvDocument {
        modified_at: None,
        file_name: "units.csv"
            .to_string(),
        headers: vec![
            "Number".to_string(),
            "Unit_Group".to_string(),
            "Category".to_string(),
        ],
        rows: Vec::new(),
    };

    let state = uploaded_state(
        "s1",
        vec![unit_doc],
    );

    let response = discover(
        State(state),
        Json(DiscoverRequest {
            session_id: "s1"
                .to_string(),
        }),
    )
    .await;

    let bytes = axum::body::to_bytes(
        response.into_body(),
        usize::MAX,
    )
    .await
    .unwrap();

    let body: serde_json::Value =
        serde_json::from_slice(
            &bytes,
        )
        .unwrap();

    assert_eq!(
        body["unit_files_found"],
        1
    );

    assert_eq!(
        body["detected_vendor_name"],
        "QSX"
    );
}

/// DoorSwap's own raw header vocabulary (`Unit`, `Unit Type`, ...) never
/// populates the canonical `Number`/`UnitGroup` columns until confirmed —
/// this is the whole reason vendor presets must be hand-authored rather
/// than derived by matching names against the canonical field list.
#[tokio::test]
async fn discover_detects_door_swap_and_confirm_maps_unit_type_to_unitgroup(
) {
    let unit_doc = CsvDocument {
        modified_at: None,
        file_name: "Units List.csv".to_string(),
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
    };

    let state = uploaded_state("s1", vec![unit_doc]);

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

    assert_eq!(body["detected_vendor_name"], "DoorSwap");
    assert_eq!(body["requires_format_resolution"], true);

    let confirm_response = resolve_unit_format(
        State(state),
        Json(ResolveUnitFormatRequest {
            session_id: "s1".to_string(),
            action: ResolveAction::Confirm,
        }),
    )
    .await;

    let bytes = axum::body::to_bytes(confirm_response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["ready"], true);
    assert_eq!(
        body["discovered_group_names"],
        serde_json::json!(["10x10 Non-Climate Controlled (10 x 10 x 8)"])
    );
}
