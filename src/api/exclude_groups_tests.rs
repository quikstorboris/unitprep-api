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
    let response = exclude_groups(
        State(empty_state()),
        Json(ExcludeGroupsRequest {
            session_id: "missing".to_string(),
            group_names: vec!["Hertz Office Space".to_string()],
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

    let response = exclude_groups(
        State(state),
        Json(ExcludeGroupsRequest {
            session_id: "s1".to_string(),
            group_names: vec!["Hertz Office Space".to_string()],
            excluded: true,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

/// The core scenario: excluding several groups in one call drops every
/// unit in each of them from validation -- not just the first one.
#[tokio::test]
async fn excluding_multiple_groups_removes_all_of_their_units() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![
                ["A01", "Hertz Office Space", "", ""],
                ["B01", "Boat Slip", "", ""],
                ["A02", "10x10 Inside Climate", "10", "10"],
            ],
        )],
    );

    let response = exclude_groups(
        State(state),
        Json(ExcludeGroupsRequest {
            session_id: "s1".to_string(),
            group_names: vec!["Hertz Office Space".to_string(), "Boat Slip".to_string()],
            excluded: true,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;

    let issues = body["issues"].as_array().unwrap();

    for issue in issues {
        let group_names: Vec<&str> = issue["affected_group_names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();

        assert!(!group_names.contains(&"Hertz Office Space"));
        assert!(!group_names.contains(&"Boat Slip"));
    }
}

/// Setting `excluded: false` undoes the exclusion for every named group
/// at once.
#[tokio::test]
async fn including_multiple_groups_again_restores_all_of_their_units() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![
                ["A01", "Hertz Office Space", "", ""],
                ["B01", "Boat Slip", "", ""],
            ],
        )],
    );

    exclude_groups(
        State(state.clone()),
        Json(ExcludeGroupsRequest {
            session_id: "s1".to_string(),
            group_names: vec!["Hertz Office Space".to_string(), "Boat Slip".to_string()],
            excluded: true,
        }),
    )
    .await;

    let response = exclude_groups(
        State(state),
        Json(ExcludeGroupsRequest {
            session_id: "s1".to_string(),
            group_names: vec!["Hertz Office Space".to_string(), "Boat Slip".to_string()],
            excluded: false,
        }),
    )
    .await;

    let body = body_json(response).await;

    let issues = body["issues"].as_array().unwrap();

    let both_back = ["Hertz Office Space", "Boat Slip"].iter().all(|name| {
        issues.iter().any(|issue| {
            issue["affected_group_names"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str() == Some(name))
        })
    });

    assert!(both_back);
}
