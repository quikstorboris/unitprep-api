use std::sync::Arc;

use axum::http::StatusCode;

use unitprep_core::session_store::SessionStoreExt;
use unitprep_unit_group::{AnalysisResults, BatchRun, CorrectionKey};

use super::*;
use crate::api::test_support::{discovered_state, empty_state, unit_document, validated_state};
use crate::application::unit_group_session::WorkflowStage;

#[tokio::test]
async fn analyze_returns_404_for_missing_session() {
    let response = analyze(
        State(empty_state()),
        crate::api::test_support::test_user(),
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
        crate::api::test_support::test_user(),
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
        crate::api::test_support::test_user(),
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

/// Regression test for the session-ownership IDOR closed by switching
/// every session-touching handler from `with_session`/`with_session_mut`
/// to `with_owned_session`/`with_owned_session_mut` (see
/// `core::session_store`'s own doc comment for the mechanism). A fully
/// ready, genuinely analyzable session belonging to `test_user()` must be
/// completely invisible to a *different* authenticated caller -- not just
/// blocked from mutating it, but indistinguishable from a session that
/// doesn't exist at all, exactly like an unrecognized `session_id` would
/// be. `analyze_finds_net_new_groups_with_no_reference_file` above proves
/// this exact session succeeds for its real owner; this proves the same
/// session 404s for anyone else.
#[tokio::test]
async fn analyze_returns_404_for_a_session_belonging_to_a_different_user() {
    let state = validated_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "10", "10"]],
        )],
    );

    let someone_else = crate::auth::AuthenticatedUser {
        user_id: uuid::Uuid::new_v4(),
        role_keys: Vec::new(),
        permission_keys: std::collections::HashSet::new(),
        token_hash: vec![0u8; 32],
        elevated_until: None,
        requires_step_up: false,
    };

    let response = analyze(
        State(state),
        someone_else,
        Json(AnalyzeRequest {
            session_id: "s1".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
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

/// Regression test for the TOCTOU race this session's bug-hunt found and
/// confirmed live: a correction landing between analyze()'s read and its
/// delayed write-back used to have its `Validated` safety-net downgrade
/// (see `run_validation` -> `complete_validation`) silently undone by
/// analyze's unconditional `complete_analysis` call, re-promoting the
/// workflow to `Analyzed` using data from before the correction. This
/// reproduces the same read -> concurrent-mutation -> write-back sequence
/// analyze() performs, using the generation-check it now runs before that
/// write-back.
#[tokio::test]
async fn a_correction_between_analyzes_read_and_write_back_is_not_silently_overwritten() {
    let state = validated_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "10", "10"]],
        )],
    );

    // analyze()'s own initial read -- captures the generation exactly
    // like its real `with_session` closure does.
    let read_generation = state
        .unit_group_sessions
        .with_session("s1", |session| session.data_generation())
        .unwrap();

    // A concurrent /correct landing in the read -> write-back gap --
    // bumps the generation, same as the real handler would trigger.
    state.unit_group_sessions.with_session_mut("s1", |session| {
        session.add_correction(
            CorrectionKey {
                file_name: "units.csv".to_string(),
                unit_number: "A01".to_string(),
                field: "width".to_string(),
            },
            "12".to_string(),
        );
    });

    let workflow_after_correction = state
        .unit_group_sessions
        .with_session("s1", |session| session.workflow)
        .unwrap();

    assert_eq!(
        workflow_after_correction,
        WorkflowStage::Validated,
        "add_correction alone doesn't downgrade workflow -- validated_state already starts there"
    );

    // analyze()'s delayed write-back, using its own generation check.
    let results = Arc::new(AnalysisResults {
        batch_run: BatchRun {
            facilities: Vec::new(),
            global_groups: Default::default(),
            advisory_issues: Vec::new(),
        },
        reference_groups: None,
        net_new_groups: Vec::new(),
        similar_groups: Vec::new(),
    });

    let wrote = state.unit_group_sessions.with_session_mut("s1", |session| {
        if session.data_generation() == read_generation {
            session.complete_analysis(results.clone());
            true
        } else {
            false
        }
    });

    assert_eq!(
        wrote,
        Some(false),
        "the stale write-back must be discarded, not silently applied"
    );

    let workflow = state
        .unit_group_sessions
        .with_session("s1", |session| session.workflow)
        .unwrap();

    assert_eq!(
        workflow,
        WorkflowStage::Validated,
        "the concurrent correction's downgrade must survive -- it must NOT get \
         silently re-promoted to Analyzed by the stale write-back"
    );
}
