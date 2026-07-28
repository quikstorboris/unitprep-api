use unitprep_core::session_store::SessionStoreExt;
use unitprep_unit_group::{CorrectionKey, ValidationResult};

use super::*;
use crate::api::test_support::{
    analyzed_state_with_errors, empty_state, unit_document, validated_state,
};
use crate::application::unit_group_session::WorkflowStage;

#[tokio::test]
async fn export_returns_404_for_missing_session() {
    let response = export(
        State(empty_state()),
        Json(ExportRequest {
            session_id: "missing".to_string(),
            acknowledge_errors: false,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// Regression test for the stage/error inconsistency fix: calling
/// `/export` before `/analyze` must return 409, consistent with
/// `/validate` and `/analyze`'s own stage-violation responses —
/// previously this specific case used a bespoke plain-text 400
/// rather than the shared structured 409 the other endpoints use.
#[tokio::test]
async fn export_returns_409_when_called_before_analysis() {
    let state = validated_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "10", "10"]],
        )],
    );

    let response = export(
        State(state),
        Json(ExportRequest {
            session_id: "s1".to_string(),
            acknowledge_errors: false,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn export_blocked_without_acknowledge_when_errors_present() {
    let state = analyzed_state_with_errors(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "", ""]],
        )],
    );

    let response = export(
        State(state),
        Json(ExportRequest {
            session_id: "s1".to_string(),
            acknowledge_errors: false,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn export_succeeds_with_acknowledge_despite_errors() {
    let state = analyzed_state_with_errors(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "", ""]],
        )],
    );

    let response = export(
        State(state),
        Json(ExportRequest {
            session_id: "s1".to_string(),
            acknowledge_errors: true,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response.headers().get(header::CONTENT_TYPE).unwrap();

    assert_eq!(content_type, "application/zip");
}

/// Same narrow write-back race as `analyze.rs` (see that module's
/// equivalent test for the full reasoning on why this is a direct
/// store-level check rather than a timing-based concurrency test): if
/// the session vanishes between export's read lock and this write-back,
/// `with_session_mut` must report "gone" without running its closure.
#[tokio::test]
async fn write_back_after_session_deletion_is_detected_not_run() {
    let state = analyzed_state_with_errors(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "10", "10"]],
        )],
    );

    state.unit_group_sessions.delete("s1");

    let wrote = state
        .unit_group_sessions
        .with_session_mut("s1", |_session| {
            unreachable!("with_session_mut must not invoke its closure for a deleted session");
        });

    assert!(wrote.is_none());
}

/// Regression test for the TOCTOU race this session's bug-hunt found: a
/// correction landing between export()'s read and its delayed write-back
/// used to have its `Validated` safety-net downgrade silently undone by
/// export's unconditional `complete_export` call, re-promoting the
/// workflow to `Exported` even though the ZIP just returned was built
/// from data that's no longer current. See analyze_tests.rs's equivalent
/// test for the full reasoning on this store-level reproduction style.
#[tokio::test]
async fn a_correction_between_exports_read_and_write_back_is_not_silently_overwritten() {
    let state = analyzed_state_with_errors(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "10", "10"]],
        )],
    );

    // export()'s own initial read -- captures the generation exactly
    // like its real `with_session` closure does.
    let read_generation = state
        .unit_group_sessions
        .with_session("s1", |session| session.data_generation())
        .unwrap();

    // A concurrent /correct landing in the read -> write-back gap --
    // bumps the generation and downgrades workflow back to Validated,
    // the same as the real `/correct` handler's own safety net.
    state.unit_group_sessions.with_session_mut("s1", |session| {
        session.add_correction(
            CorrectionKey {
                file_name: "units.csv".to_string(),
                unit_number: "A01".to_string(),
                field: "width".to_string(),
            },
            "12".to_string(),
        );

        session.complete_validation(ValidationResult {
            files_checked: 1,
            issue_count: 0,
            error_count: 0,
            warning_count: 0,
            issues: Vec::new(),
            files_errored: Vec::new(),
            ready: true,
        });
    });

    // export()'s delayed write-back, using its own generation check.
    let wrote = state.unit_group_sessions.with_session_mut("s1", |session| {
        if session.data_generation() == read_generation {
            session.complete_export();
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
         silently re-promoted to Exported by the stale write-back"
    );
}
