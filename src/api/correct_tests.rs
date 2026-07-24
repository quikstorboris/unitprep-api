use axum::http::StatusCode;

use super::*;
use crate::api::test_support::{
    discovered_state,
    empty_state,
    unit_document,
    uploaded_state,
};

#[tokio::test]
async fn correct_returns_404_for_missing_session(
) {
    let response = correct(
        State(empty_state()),
        Json(CorrectRequest {
            session_id: "missing"
                .to_string(),
            file_name: "units.csv"
                .to_string(),
            unit_number: "A01"
                .to_string(),
            field: "width"
                .to_string(),
            value: "10"
                .to_string(),
        }),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND
    );
}

/// Regression test for the stage/error inconsistency fix: `/correct`
/// re-runs validation internally, so it must surface the same 409
/// (not a fake 200) when the session hasn't been discovered yet.
#[tokio::test]
async fn correct_returns_409_when_called_before_discovery(
) {
    let state = uploaded_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![[
                "A01",
                "10x10 Inside Climate",
                "",
                "",
            ]],
        )],
    );

    let response = correct(
        State(state),
        Json(CorrectRequest {
            session_id: "s1"
                .to_string(),
            file_name: "units.csv"
                .to_string(),
            unit_number: "A01"
                .to_string(),
            field: "width"
                .to_string(),
            value: "10"
                .to_string(),
        }),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn correct_clears_invalid_dimensions_warning(
) {
    // UnitGroup must actually parse as a real dimension — an odd/
    // non-dimensioned name like "1200 sq ft" is now excluded from this
    // check entirely regardless of its actual columns (see the comment
    // on the equivalent validate.rs test for why).
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![
                [
                    "Office",
                    "10x10 Inside Climate",
                    "",
                    "",
                ],
                [
                    "Office2",
                    "10x10 Inside Climate",
                    "10",
                    "10",
                ],
            ],
        )],
    );

    // Fix width first — length is still blank, so the warning should
    // still be present until both are corrected.
    let response = correct(
        State(state.clone()),
        Json(CorrectRequest {
            session_id: "s1"
                .to_string(),
            file_name: "units.csv"
                .to_string(),
            unit_number: "Office"
                .to_string(),
            field: "width"
                .to_string(),
            value: "10"
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

    let issues = body["issues"]
        .as_array()
        .unwrap();

    assert!(issues.iter().any(|i| {
        i["description"]
            == "Invalid dimensions"
    }));

    // Now fix length too — the warning should clear entirely.
    let response = correct(
        State(state),
        Json(CorrectRequest {
            session_id: "s1"
                .to_string(),
            file_name: "units.csv"
                .to_string(),
            unit_number: "Office"
                .to_string(),
            field: "length"
                .to_string(),
            value: "10"
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
        body["error_count"], 0
    );

    assert_eq!(
        body["ready"], true
    );
}
