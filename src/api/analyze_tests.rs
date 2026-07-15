use axum::http::StatusCode;

use super::*;
use crate::api::test_support::{
    discovered_state,
    empty_state,
    unit_document,
    validated_state,
};

#[tokio::test]
async fn analyze_returns_404_for_missing_session(
) {
    let response = analyze(
        State(empty_state()),
        Json(AnalyzeRequest {
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

/// Regression test for the stage/error inconsistency fix: calling
/// `/analyze` before `/validate` must return a distinct 409, not the
/// fake all-zero 200 success this used to return (indistinguishable
/// from "validated and genuinely found zero net-new/similar groups").
#[tokio::test]
async fn analyze_returns_409_when_called_before_validation(
) {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![[
                "A01",
                "10x10 Inside Climate",
                "10",
                "10",
            ]],
        )],
    );

    let response = analyze(
        State(state),
        Json(AnalyzeRequest {
            session_id: "s1"
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
async fn analyze_finds_net_new_groups_with_no_reference_file(
) {
    let state = validated_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![[
                "A01",
                "10x10 Inside Climate",
                "10",
                "10",
            ]],
        )],
    );

    let response = analyze(
        State(state),
        Json(AnalyzeRequest {
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

    // No master group file was selected, so every group found is
    // net-new by definition (see analyze_batch).
    assert_eq!(
        body["net_new_groups"], 1
    );

    assert_eq!(
        body["net_new_group_details"]
            [0],
        "10x10 Inside Climate"
    );
}
