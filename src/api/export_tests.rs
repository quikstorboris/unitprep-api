use unitprep_core::session_store::SessionStoreExt;
use unitprep_unit_group::{CorrectionKey, ValidationResult};

use super::*;
use crate::api::test_support::{
    analyzed_state_ready_for_export, analyzed_state_with_errors, empty_state, uploaded_state,
    unit_document, validated_state,
};
use crate::application::unit_group_session::WorkflowStage;

#[tokio::test]
async fn save_location_returns_404_for_missing_session() {
    let response = save_location(
        State(empty_state()),
        crate::api::test_support::test_user(),
        Json(ExportRequest {
            session_id: "missing".to_string(),
            client_id: None,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn save_location_is_none_for_a_locally_uploaded_session() {
    let state = uploaded_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "10", "10"]],
        )],
    );

    let response = save_location(
        State(state),
        crate::api::test_support::test_user(),
        Json(ExportRequest {
            session_id: "s1".to_string(),
            client_id: None,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["default_folder_path"], serde_json::Value::Null);
}

#[tokio::test]
async fn save_location_defaults_to_a_group_prep_output_subfolder_of_the_source_folder() {
    let state = uploaded_state(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "10", "10"]],
        )],
    );
    state.unit_group_sessions.with_session_mut("s1", |session| {
        session.data.source_dropbox_folder_path =
            Some("/qms onboarding/prairie enterprises llc/highway 20".to_string());
    });

    let response = save_location(
        State(state),
        crate::api::test_support::test_user(),
        Json(ExportRequest {
            session_id: "s1".to_string(),
            client_id: None,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        body["default_folder_path"],
        "/qms onboarding/prairie enterprises llc/highway 20/Group Prep Output"
    );
}

#[tokio::test]
async fn export_to_dropbox_rejects_a_path_outside_the_configured_root() {
    let response = export_to_dropbox(
        State(empty_state()),
        crate::api::test_support::test_user(),
        Json(ExportToDropboxRequest {
            session_id: "s1".to_string(),
            client_id: None,
            dropbox_path: "/Not/Under/The/Configured/Root/output.zip".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn export_returns_404_for_missing_session() {
    let response = export(
        State(empty_state()),
        crate::api::test_support::test_user(),
        Json(ExportRequest {
            session_id: "missing".to_string(),
            client_id: None,
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
        crate::api::test_support::test_user(),
        Json(ExportRequest {
            session_id: "s1".to_string(),
            client_id: None,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
}

/// Regression coverage for the removed `acknowledge_errors` override:
/// unresolved `Severity::Error` validation issues now block export
/// unconditionally -- there is no longer any request field that can
/// bypass this.
#[tokio::test]
async fn export_blocked_when_errors_present() {
    let state = analyzed_state_with_errors(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "", ""]],
        )],
    );

    let response = export(
        State(state),
        crate::api::test_support::test_user(),
        Json(ExportRequest {
            session_id: "s1".to_string(),
            client_id: None,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// The genuine success path: a session with clean validation (nothing
/// to block on) and non-empty analysis results must actually produce a
/// ZIP. This used to be covered incidentally by a now-removed
/// `acknowledge_errors: true` test that exercised the override path
/// instead of a genuinely clean one -- with that field gone, this is
/// the one remaining test proving `/export` ever succeeds at all.
#[tokio::test]
async fn export_succeeds_with_clean_validation() {
    let state = analyzed_state_ready_for_export(
        "s1",
        vec![unit_document(
            "units.csv",
            vec![["A01", "10x10 Inside Climate", "10", "10"]],
        )],
    );

    let response = export(
        State(state),
        crate::api::test_support::test_user(),
        Json(ExportRequest {
            session_id: "s1".to_string(),
            client_id: None,
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
