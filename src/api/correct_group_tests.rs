use axum::http::StatusCode;

use super::*;
use crate::api::test_support::{discovered_state, empty_state, unit_document, uploaded_state};

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

fn request(
    group_name: &str,
    width: Option<&str>,
    length: Option<&str>,
    additional_properties: Option<&str>,
) -> CorrectGroupRequest {
    CorrectGroupRequest {
        session_id: "s1".to_string(),
        group_name: group_name.to_string(),
        width: width.map(str::to_string),
        length: length.map(str::to_string),
        additional_properties: additional_properties.map(str::to_string),
    }
}

#[tokio::test]
async fn returns_404_for_missing_session() {
    let response = correct_group(
        State(empty_state()),
        Json(request("Hertz Office Space", Some("10"), Some("15"), None)),
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

    let response = correct_group(
        State(state),
        Json(request("Hertz Office Space", Some("10"), Some("15"), None)),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn rejects_a_group_name_that_matches_no_unit() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "10", "10"]],
        )],
    );

    let response = correct_group(
        State(state),
        Json(request("Not A Real Group", Some("10"), Some("15"), None)),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = body_json(response).await;

    assert_eq!(body["error"], "unknown_group");
}

/// Regression test: submitting the exact same rename twice must reach
/// the same successful outcome both times, not 200-then-400 -- the
/// second call finds zero units still under the *old* name (since the
/// first call already renamed them), which used to be indistinguishable
/// from a genuinely unknown group name.
#[tokio::test]
async fn repeating_the_same_rename_is_a_no_op_success_not_an_error() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "Hertz Office Space", "", ""]],
        )],
    );

    let first = correct_group(
        State(state.clone()),
        Json(request("Hertz Office Space", Some("10"), Some("15"), None)),
    )
    .await;

    assert_eq!(first.status(), StatusCode::OK);

    let second = correct_group(
        State(state),
        Json(request("Hertz Office Space", Some("10"), Some("15"), None)),
    )
    .await;

    assert_eq!(
        second.status(),
        StatusCode::OK,
        "repeating an already-applied rename should succeed as a no-op, not 400"
    );
}

/// The core scenario: width+length given, no additional properties --
/// the new UnitGroup value becomes a plain "WxL" dimension string.
#[tokio::test]
async fn renames_using_width_and_length_only() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "Hertz Office Space", "", ""]],
        )],
    );

    let response = correct_group(
        State(state),
        Json(request("Hertz Office Space", Some("10"), Some("15"), None)),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;
    let issues = body["issues"].as_array().unwrap();

    // The single unit's group is now "10x15", not "Hertz Office Space"
    // -- every issue's `affected_group_names` must reflect the rename.
    for issue in issues {
        let group_names: Vec<&str> = issue["affected_group_names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();

        assert!(!group_names.contains(&"Hertz Office Space"));
    }

    let renamed_group_appears = issues.iter().any(|issue| {
        issue["affected_group_names"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("10x15"))
    });

    assert!(renamed_group_appears);
}

/// Width/Length are optional -- an odd/non-dimensioned group can be
/// renamed by Additional Properties alone, appended to the *existing*
/// group name rather than inventing dimensions.
#[tokio::test]
async fn renames_using_additional_properties_only_appends_to_existing_name() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "Hertz Office Space", "", ""]],
        )],
    );

    correct_group(
        State(state.clone()),
        Json(request(
            "Hertz Office Space",
            None,
            None,
            Some("Ground Floor"),
        )),
    )
    .await;

    // Re-run correct-group on the ORIGINAL name again -- if the rename
    // actually took effect, no unit still has "Hertz Office Space" as
    // its UnitGroup, so this second call should now find nothing to
    // rename.
    let response = correct_group(
        State(state),
        Json(request(
            "Hertz Office Space",
            None,
            None,
            Some("Second Floor"),
        )),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// The core new capability: renaming applies to every unit sharing the
/// group name at once, not one unit at a time.
#[tokio::test]
async fn renames_every_unit_sharing_the_group_name() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![
                ["A01", "Hertz Office Space", "", ""],
                ["A02", "Hertz Office Space", "", ""],
                ["A03", "10x10 Inside Climate", "10", "10"],
            ],
        )],
    );

    correct_group(
        State(state.clone()),
        Json(request(
            "Hertz Office Space",
            Some("12"),
            Some("12"),
            Some("Ground Floor"),
        )),
    )
    .await;

    // Both A01 and A02 were renamed -- a second correct-group call on
    // the original name now matches nothing.
    let response = correct_group(
        State(state),
        Json(request("Hertz Office Space", None, None, None)),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
