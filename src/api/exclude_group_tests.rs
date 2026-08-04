use axum::http::StatusCode;

use super::*;
use crate::api::test_support::{discovered_state, empty_state, unit_document, uploaded_state};

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn returns_404_for_missing_session() {
    let response = exclude_group(
        State(empty_state()),
        crate::api::test_support::test_user(),
        Json(ExcludeGroupRequest {
            session_id: "missing".to_string(),
            group_name: "Hertz Office Space".to_string(),
            excluded: true,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn returns_409_before_discovery_completes() {
    let state = uploaded_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "Hertz Office Space", "", ""]],
        )],
    );

    let response = exclude_group(
        State(state),
        crate::api::test_support::test_user(),
        Json(ExcludeGroupRequest {
            session_id: "s1".to_string(),
            group_name: "Hertz Office Space".to_string(),
            excluded: true,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

/// The core scenario: excluding a group drops every unit in it from
/// validation entirely -- not just the one issue it happened to raise.
#[tokio::test]
async fn excluding_a_group_removes_its_units_from_validation() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![
                ["A01", "Hertz Office Space", "", ""],
                ["A02", "10x10 Inside Climate", "10", "10"],
            ],
        )],
    );

    let response = exclude_group(
        State(state),
        crate::api::test_support::test_user(),
        Json(ExcludeGroupRequest {
            session_id: "s1".to_string(),
            group_name: "Hertz Office Space".to_string(),
            excluded: true,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;

    // "Hertz Office Space" (the excluded group) no longer shows up
    // anywhere -- it's odd and rare, so if it were still present it
    // would raise its own "Odd UnitGroup values"/"Rare UnitGroup
    // detected" warnings, flagged by group name (see
    // `flagged_are_group_names`), not by "A01".
    let issues = body["issues"].as_array().unwrap();

    for issue in issues {
        let group_names: Vec<&str> = issue["affected_group_names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();

        assert!(!group_names.contains(&"Hertz Office Space"));
    }
}

/// Setting `excluded: false` undoes a previous exclusion -- the group's
/// units are back in scope for the next validation run.
#[tokio::test]
async fn including_a_group_again_restores_its_units() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "Hertz Office Space", "", ""]],
        )],
    );

    exclude_group(
        State(state.clone()),
        crate::api::test_support::test_user(),
        Json(ExcludeGroupRequest {
            session_id: "s1".to_string(),
            group_name: "Hertz Office Space".to_string(),
            excluded: true,
        }),
    )
    .await;

    let response = exclude_group(
        State(state),
        crate::api::test_support::test_user(),
        Json(ExcludeGroupRequest {
            session_id: "s1".to_string(),
            group_name: "Hertz Office Space".to_string(),
            excluded: false,
        }),
    )
    .await;

    let body = body_json(response).await;

    let issues = body["issues"].as_array().unwrap();

    // "Hertz Office Space" is odd (no dimension attempt at all), so it's
    // flagged by group name, not by "A01" -- check `affected_group_names`
    // rather than `affected_unit_ids`, which for this per-group check
    // holds the group name too (see `flagged_are_group_names`).
    let group_is_back = issues.iter().any(|issue| {
        issue["affected_group_names"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("Hertz Office Space"))
    });

    assert!(group_is_back);
}
