use axum::http::StatusCode;

use unitprep_core::session_store::SessionStoreExt;

use super::*;
use crate::api::test_support::{discovered_state, empty_state, unit_document, validated_state};

#[tokio::test]
async fn analyze_returns_404_for_missing_session() {
    let response = analyze(
        State(empty_state()),
        Json(AnalyzeRequest {
            session_id: "missing".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Regression test for the stage/error inconsistency fix: calling
/// `/analyze` before `/validate` must return a distinct 409, not the
/// fake all-zero 200 success this used to return (indistinguishable
/// from "validated and genuinely found zero net-new/similar groups").
#[tokio::test]
async fn analyze_returns_409_when_called_before_validation() {
    let state = discovered_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "10", "10"]],
        )],
    );

    let response = analyze(
        State(state),
        Json(AnalyzeRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn analyze_finds_net_new_groups_with_no_reference_file() {
    let state = validated_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "10", "10"]],
        )],
    );

    let response = analyze(
        State(state),
        Json(AnalyzeRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();

    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    // No master group file was selected, so every group found is
    // net-new by definition (see analyze_batch).
    assert_eq!(body["net_new_groups"], 1);

    assert_eq!(body["net_new_group_details"][0], "10x10 Inside Climate");
}

/// Regression test for the session write-back race documented on
/// `analyze()`'s `with_session_mut` call: if the session vanishes
/// (expires, or is explicitly cancelled by a concurrent request) in the
/// narrow window between the earlier read lock and this write-back,
/// `with_session_mut` must return `None` cleanly -- not panic, and not
/// silently run its closure against a resurrected or wrong session.
///
/// A genuine multi-threaded race here is nanoseconds-to-microseconds
/// wide (there's no `.await` between the two lock acquisitions for a
/// concurrent task to interleave into) and isn't reliably forceable from
/// a test without refactoring the handler for an injectable pause point.
/// This exercises the exact store-level contract the handler's
/// race-safety branch depends on directly: deleting the session first,
/// then confirming `with_session_mut` reports "gone" instead of running
/// the write-back closure at all.
#[tokio::test]
async fn write_back_after_session_deletion_is_detected_not_run() {
    let state = validated_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "10", "10"]],
        )],
    );

    // Simulates the session vanishing between analyze()'s read and its
    // write-back -- exactly the window the race in question occupies.
    state.unit_group_sessions.delete("s1");

    let wrote = state
        .unit_group_sessions
        .with_session_mut("s1", |_session| {
            unreachable!("with_session_mut must not invoke its closure for a deleted session");
        });

    assert!(wrote.is_none());
}
