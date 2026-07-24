use axum::http::StatusCode;

use unitprep_unit_group::{ODD_UNITGROUP, RARE_GROUP};

use super::*;
use crate::api::test_support::{
    discovered_state,
    empty_state,
    unit_document,
    uploaded_state,
};

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

fn has_group(issues: &[serde_json::Value], description: &str, group_name: &str) -> bool {
    issues.iter().any(|issue| {
        issue["description"] == description
            && issue["affected_group_names"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str() == Some(group_name))
    })
}

#[tokio::test]
async fn returns_404_for_missing_session() {
    let response = acknowledge_group_warnings(
        State(empty_state()),
        Json(AcknowledgeGroupWarningsRequest {
            session_id: "missing".to_string(),
            check: ODD_UNITGROUP.to_string(),
            group_names: vec!["Hertz Office Space".to_string()],
            acknowledged: true,
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

    let response = acknowledge_group_warnings(
        State(state),
        Json(AcknowledgeGroupWarningsRequest {
            session_id: "s1".to_string(),
            check: ODD_UNITGROUP.to_string(),
            group_names: vec!["Hertz Office Space".to_string()],
            acknowledged: true,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

/// The core scenario: a group that's both odd (no dimension attempt) and
/// rare (a single unit) gets acknowledged for Odd only -- it must stop
/// appearing under "Odd UnitGroup values" while still being flagged
/// under "Rare UnitGroup detected", and its unit must still be present in
/// the data (not excluded).
#[tokio::test]
async fn acknowledging_a_group_hides_it_from_that_check_only() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "Hertz Office Space", "", ""]],
        )],
    );

    let response = acknowledge_group_warnings(
        State(state),
        Json(AcknowledgeGroupWarningsRequest {
            session_id: "s1".to_string(),
            check: ODD_UNITGROUP.to_string(),
            group_names: vec!["Hertz Office Space".to_string()],
            acknowledged: true,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;
    let issues = body["issues"].as_array().unwrap();

    assert!(!has_group(issues, ODD_UNITGROUP, "Hertz Office Space"));
    assert!(has_group(issues, RARE_GROUP, "Hertz Office Space"));
}

/// Setting `acknowledged: false` undoes it -- the group is flagged again
/// under the check it was acknowledged for.
#[tokio::test]
async fn unacknowledging_restores_the_warning() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "Hertz Office Space", "", ""]],
        )],
    );

    acknowledge_group_warnings(
        State(state.clone()),
        Json(AcknowledgeGroupWarningsRequest {
            session_id: "s1".to_string(),
            check: ODD_UNITGROUP.to_string(),
            group_names: vec!["Hertz Office Space".to_string()],
            acknowledged: true,
        }),
    )
    .await;

    let response = acknowledge_group_warnings(
        State(state),
        Json(AcknowledgeGroupWarningsRequest {
            session_id: "s1".to_string(),
            check: ODD_UNITGROUP.to_string(),
            group_names: vec!["Hertz Office Space".to_string()],
            acknowledged: false,
        }),
    )
    .await;

    let body = body_json(response).await;
    let issues = body["issues"].as_array().unwrap();

    assert!(has_group(issues, ODD_UNITGROUP, "Hertz Office Space"));
}

/// Acknowledging a group for Rare specifically must not also suppress it
/// under Odd -- the two checks are independent.
#[tokio::test]
async fn acknowledgment_is_scoped_to_the_named_check() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "Hertz Office Space", "", ""]],
        )],
    );

    let response = acknowledge_group_warnings(
        State(state),
        Json(AcknowledgeGroupWarningsRequest {
            session_id: "s1".to_string(),
            check: RARE_GROUP.to_string(),
            group_names: vec!["Hertz Office Space".to_string()],
            acknowledged: true,
        }),
    )
    .await;

    let body = body_json(response).await;
    let issues = body["issues"].as_array().unwrap();

    assert!(!has_group(issues, RARE_GROUP, "Hertz Office Space"));
    assert!(has_group(issues, ODD_UNITGROUP, "Hertz Office Space"));
}
