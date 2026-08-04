use axum::http::StatusCode;

use super::*;
use crate::api::test_support::{discovered_state, empty_state, unit_document, uploaded_state};

#[tokio::test]
async fn correct_returns_404_for_missing_session() {
    let response = correct(
        State(empty_state()),
        crate::api::test_support::test_user(),
        Json(CorrectRequest {
            session_id: "missing".to_string(),
            file_name: "units.csv".to_string(),
            unit_number: "A01".to_string(),
            field: "width".to_string(),
            value: "10".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Regression test for the stage/error inconsistency fix: `/correct`
/// re-runs validation internally, so it must surface the same 409
/// (not a fake 200) when the session hasn't been discovered yet.
#[tokio::test]
async fn correct_returns_409_when_called_before_discovery() {
    let state = uploaded_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "", ""]],
        )],
    );

    let response = correct(
        State(state),
        crate::api::test_support::test_user(),
        Json(CorrectRequest {
            session_id: "s1".to_string(),
            file_name: "units.csv".to_string(),
            unit_number: "A01".to_string(),
            field: "width".to_string(),
            value: "10".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

/// Regression test: a `unit_number` that doesn't exist in `file_name` at
/// all (a stale identifier, a typo, or a unit whose group was excluded
/// since the UI last loaded) must be rejected rather than silently
/// stored as a dead correction with no effect and no error.
#[tokio::test]
async fn correct_rejects_a_unit_number_that_does_not_exist_in_the_file() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "10", "10"]],
        )],
    );

    let response = correct(
        State(state),
        crate::api::test_support::test_user(),
        Json(CorrectRequest {
            session_id: "s1".to_string(),
            file_name: "units.csv".to_string(),
            unit_number: "NOT-A-REAL-UNIT".to_string(),
            field: "width".to_string(),
            value: "10".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["error"], "unknown_unit");
}

/// Regression test: two rows sharing the same unit number (already
/// flagged as a Duplicate Unit Numbers error) must reject a `/correct`
/// naming that unit number, rather than silently applying the same new
/// value to both rows -- including the one that was already correct.
#[tokio::test]
async fn correct_rejects_an_ambiguous_duplicate_unit_number() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![
                ["A01", "10x10 Inside Climate", "0", "10"],
                ["A01", "10x10 Inside Climate", "10", "10"],
            ],
        )],
    );

    let response = correct(
        State(state),
        crate::api::test_support::test_user(),
        Json(CorrectRequest {
            session_id: "s1".to_string(),
            file_name: "units.csv".to_string(),
            unit_number: "A01".to_string(),
            field: "width".to_string(),
            value: "10".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["error"], "ambiguous_unit_number");
}

/// A unique unit number in the same file must still correct normally --
/// the ambiguity check is scoped to genuinely duplicated numbers only.
#[tokio::test]
async fn correct_still_applies_for_a_unique_unit_number_alongside_a_duplicate() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![
                ["A01", "10x10 Inside Climate", "0", "10"],
                ["A01", "10x10 Inside Climate", "10", "10"],
                ["A02", "10x10 Inside Climate", "0", "10"],
            ],
        )],
    );

    let response = correct(
        State(state),
        crate::api::test_support::test_user(),
        Json(CorrectRequest {
            session_id: "s1".to_string(),
            file_name: "units.csv".to_string(),
            unit_number: "A02".to_string(),
            field: "width".to_string(),
            value: "10".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn correct_clears_invalid_dimensions_warning() {
    // UnitGroup must actually parse as a real dimension — an odd/
    // non-dimensioned name like "1200 sq ft" is now excluded from this
    // check entirely regardless of its actual columns (see the comment
    // on the equivalent validate.rs test for why).
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![
                ["Office", "10x10 Inside Climate", "", ""],
                ["Office2", "10x10 Inside Climate", "10", "10"],
            ],
        )],
    );

    // Fix width first — length is still blank, so the warning should
    // still be present until both are corrected.
    let response = correct(
        State(state.clone()),
        crate::api::test_support::test_user(),
        Json(CorrectRequest {
            session_id: "s1".to_string(),
            file_name: "units.csv".to_string(),
            unit_number: "Office".to_string(),
            field: "width".to_string(),
            value: "10".to_string(),
        }),
    )
    .await;

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    let issues = body["issues"].as_array().unwrap();

    assert!(issues
        .iter()
        .any(|i| { i["description"] == "Invalid dimensions" }));

    // Now fix length too — the warning should clear entirely.
    let response = correct(
        State(state),
        crate::api::test_support::test_user(),
        Json(CorrectRequest {
            session_id: "s1".to_string(),
            file_name: "units.csv".to_string(),
            unit_number: "Office".to_string(),
            field: "length".to_string(),
            value: "10".to_string(),
        }),
    )
    .await;

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(body["error_count"], 0);

    assert_eq!(body["ready"], true);
}
