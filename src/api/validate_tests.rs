use axum::http::StatusCode;
use unitprep_core::csv_document::CsvDocument;

use super::*;
use crate::api::test_support::{
    discovered_state,
    empty_state,
    unit_document,
    uploaded_state,
};

async fn body_json(
    response: Response,
) -> serde_json::Value {
    let bytes = axum::body::to_bytes(
        response.into_body(),
        usize::MAX,
    )
    .await
    .unwrap();

    serde_json::from_slice(&bytes)
        .unwrap()
}

#[tokio::test]
async fn validate_returns_404_for_missing_session(
) {
    let response = validate(
        State(empty_state()),
        Json(ValidateRequest {
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
/// `/validate` before `/discover` must return a distinct 409, not the
/// fake all-zero 200 success this used to return (indistinguishable
/// from "discovered and genuinely found nothing to validate").
#[tokio::test]
async fn validate_returns_409_when_called_before_discovery(
) {
    let state = uploaded_state(
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

    let response = validate(
        State(state),
        Json(ValidateRequest {
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
async fn validate_reports_invalid_dimensions_as_exemptable(
) {
    // UnitGroup deliberately doesn't parse as a "WxL"-style name
    // (like the real "1200 sq ft" office repro) — a dimensioned name
    // such as "10x10 Inside Climate" would also trip the *separate*
    // "UnitGroup dimensions do not match Width/Length" check against
    // blank actual values, which isn't what this test is about.
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![[
                "Office",
                "1200 sq ft",
                "",
                "",
            ]],
        )],
    );

    let response = validate(
        State(state),
        Json(ValidateRequest {
            session_id: "s1"
                .to_string(),
        }),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::OK
    );

    let body =
        body_json(response).await;

    assert_eq!(
        body["error_count"], 1
    );

    assert_eq!(
        body["issues"][0]
            ["description"],
        "Invalid dimensions"
    );

    assert_eq!(
        body["issues"][0]
            ["exemptable"],
        true
    );
}

/// Regression test for the "aggregate loud-error" gap: if a file
/// discovery classified as a unit file turns out to be missing its
/// required columns by the time validation actually opens it (an
/// internal inconsistency, not a data-quality problem — see
/// `validate_document`'s `Err` path), the response must not look like
/// a clean/absent result. `ready` must be false and the file must be
/// named in `files_errored`, not silently skipped.
#[tokio::test]
async fn validate_reports_files_that_error_and_blocks_readiness(
) {
    let bad_document = CsvDocument {
            modified_at: None,
        file_name: "units.csv"
            .to_string(),
        headers: vec![
            "some_other_column"
                .to_string(),
        ],
        rows: vec![vec![
            "value".to_string(),
        ]],
    };

    let state = discovered_state(
        "s1",
        vec![bad_document],
    );

    let response = validate(
        State(state),
        Json(ValidateRequest {
            session_id: "s1"
                .to_string(),
        }),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::OK
    );

    let body =
        body_json(response).await;

    assert_eq!(
        body["files_checked"], 0
    );

    assert_eq!(body["ready"], false);

    let files_errored = body
        ["files_errored"]
        .as_array()
        .unwrap();

    assert_eq!(
        files_errored.len(),
        1
    );

    assert_eq!(
        files_errored[0]
            ["file_name"],
        "units.csv"
    );

    assert!(files_errored[0]
        ["message"]
        .as_str()
        .unwrap()
        .contains("units.csv"));
}
