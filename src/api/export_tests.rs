use super::*;
use crate::api::test_support::{
    analyzed_state_with_errors,
    empty_state,
    unit_document,
    validated_state,
};

#[tokio::test]
async fn export_returns_404_for_missing_session(
) {
    let response = export(
        State(empty_state()),
        Json(ExportRequest {
            session_id: "missing"
                .to_string(),
            acknowledge_errors:
                false,
        }),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND
    );
}

/// Regression test for the stage/error inconsistency fix: calling
/// `/export` before `/analyze` must return 409, consistent with
/// `/validate` and `/analyze`'s own stage-violation responses —
/// previously this specific case used a bespoke plain-text 400
/// rather than the shared structured 409 the other endpoints use.
#[tokio::test]
async fn export_returns_409_when_called_before_analysis(
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

    let response = export(
        State(state),
        Json(ExportRequest {
            session_id: "s1"
                .to_string(),
            acknowledge_errors:
                false,
        }),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn export_blocked_without_acknowledge_when_errors_present(
) {
    let state =
        analyzed_state_with_errors(
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

    let response = export(
        State(state),
        Json(ExportRequest {
            session_id: "s1"
                .to_string(),
            acknowledge_errors:
                false,
        }),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn export_succeeds_with_acknowledge_despite_errors(
) {
    let state =
        analyzed_state_with_errors(
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

    let response = export(
        State(state),
        Json(ExportRequest {
            session_id: "s1"
                .to_string(),
            acknowledge_errors:
                true,
        }),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::OK
    );

    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap();

    assert_eq!(
        content_type,
        "application/zip"
    );
}
