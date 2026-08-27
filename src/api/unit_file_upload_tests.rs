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

/// A hand-built unit-list CSV shape that matches no registered vendor
/// signature -- the real case this endpoint exists for (a net-new
/// facility's own export, not a recognized PMS format).
fn unmatched_document(file_name: &str) -> CsvDocument {
    document(
        file_name,
        vec!["UnitNumber", "SizeCode", "SizeDescription", "UnitStatus"],
    )
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn returns_404_for_missing_session() {
    let response = apply_unit_file_upload(
        &empty_state(),
        "missing",
        crate::api::test_support::test_user_id(),
        unmatched_document("units.csv"),
        &[],
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
            Some(crate::api::test_support::test_user_id()),
        ));

    let response = apply_unit_file_upload(
        &state,
        "s1",
        crate::api::test_support::test_user_id(),
        unmatched_document("units.csv"),
        &[],
    );

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

/// The core scenario the feature exists for: a net-new facility's own
/// unit-list CSV doesn't match any registered vendor signature, so
/// discovery never offers it as a candidate. Manually uploading it forces
/// it into the selected set and routes it into format resolution (manual
/// column mapping) instead of leaving it stuck as "unrecognized".
#[tokio::test]
async fn manually_uploaded_file_with_unmatched_headers_becomes_a_selected_unit_file() {
    let state = discovered_state("s1", vec![qsx_document("units.csv")]);

    let response = apply_unit_file_upload(
        &state,
        "s1",
        crate::api::test_support::test_user_id(),
        unmatched_document("boris_units.csv"),
        &crate::api::test_support::default_unit_vendors_cache().read().clone(),
    );

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;

    let selected = body["selected_unit_file_names"].as_array().unwrap();
    assert!(selected
        .iter()
        .any(|v| v.as_str() == Some("boris_units.csv")));

    assert_eq!(body["requires_format_resolution"], true);
    // "boris_units.csv" sorts before "units.csv", and neither has a
    // stored resolution yet, so it's the current file to resolve --
    // with no detected vendor, since its headers match none.
    assert_eq!(body["current_unit_file_name"], "boris_units.csv");
    assert_eq!(body["detected_vendor_name"], serde_json::Value::Null);
}

/// A second call to /discover after a manual override must not drop the
/// forced selection just because the manually chosen file doesn't
/// independently classify as a unit-file candidate -- this is the actual
/// regression `reconcile_unit_file_selection`'s preservation fix covers.
#[tokio::test]
async fn manual_selection_survives_a_subsequent_discovery_recompute() {
    let state = discovered_state("s1", vec![qsx_document("units.csv")]);

    apply_unit_file_upload(
        &state,
        "s1",
        crate::api::test_support::test_user_id(),
        unmatched_document("boris_units.csv"),
        &crate::api::test_support::default_unit_vendors_cache().read().clone(),
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

    let selected = body["selected_unit_file_names"].as_array().unwrap();
    assert!(selected
        .iter()
        .any(|v| v.as_str() == Some("boris_units.csv")));
}
