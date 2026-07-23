use super::*;
use unitprep_unit_group::{
    AnalysisResults,
    BatchRun,
    ValidationResult,
};
use unitprep_core::in_memory_session_store::InMemorySessionStore;
use unitprep_core::session::HasSessionMetadata;
use unitprep_core::session_store::SessionStore;

fn discovery_result(
) -> DiscoveryResult {
    DiscoveryResult {
        unit_file_names: vec![
            "units.csv"
                .to_string(),
        ],
        group_file_names: vec![
            "groups.csv"
                .to_string(),
        ],
        selected_group_file_name:
            Some(
                "groups.csv"
                    .to_string(),
            ),
        ready: true,
        unit_file_candidates: vec![
            unitprep_unit_group::UnitFileCandidate {
                file_name: "units.csv".to_string(),
                modified_at: None,
                detected_vendor: "QSX".to_string(),
            },
        ],
        selected_unit_file_name: Some(
            "units.csv".to_string(),
        ),
        requires_unit_file_selection: false,
        requires_format_resolution: false,
        detected_vendor_name: Some(
            "QSX".to_string(),
        ),
        source_headers: Vec::new(),
        suggested_mapping: Vec::new(),
    }
}

fn validation_result(
) -> ValidationResult {
    ValidationResult {
        files_checked: 1,
        issue_count: 0,
        error_count: 0,
        warning_count: 0,
        issues: Vec::new(),
        files_errored: Vec::new(),
        ready: true,
    }
}

fn analysis_results(
) -> AnalysisResults {
    AnalysisResults {
        batch_run: BatchRun {
            facilities:
                Vec::new(),
            global_groups:
                Default::default(),
            advisory_issues:
                Vec::new(),
        },
        reference_groups:
            None,
        net_new_groups:
            Vec::new(),
        similar_groups:
            Vec::new(),
    }
}

#[test]
fn new_session_starts_uploaded() {
    let session =
        Session::new(
            "s1".to_string(),
        );

    assert_eq!(
        session.workflow,
        WorkflowStage::Uploaded
    );

    assert!(
        session
            .require_stage(
                WorkflowStage::Uploaded
            )
            .is_ok()
    );

    assert!(
        session
            .require_stage(
                WorkflowStage::Discovered
            )
            .is_err()
    );
}

#[test]
fn stage_ordering_is_pipeline_order(
) {
    assert!(
        WorkflowStage::Uploaded
            < WorkflowStage::Discovered
    );

    assert!(
        WorkflowStage::Discovered
            < WorkflowStage::Validated
    );

    assert!(
        WorkflowStage::Validated
            < WorkflowStage::Analyzed
    );

    assert!(
        WorkflowStage::Analyzed
            < WorkflowStage::Exported
    );
}

#[test]
fn complete_discovery_advances_stage_and_stores_data(
) {
    let mut session =
        Session::new(
            "s1".to_string(),
        );

    session.complete_discovery(
        discovery_result(),
    );

    assert_eq!(
        session.workflow,
        WorkflowStage::Discovered
    );

    assert!(
        session
            .data
            .discovery
            .is_some()
    );
}

#[test]
fn require_stage_reports_current_stage_on_failure(
) {
    let mut session =
        Session::new(
            "s1".to_string(),
        );

    session.complete_discovery(
        discovery_result(),
    );

    let err = session
        .require_stage(
            WorkflowStage::Analyzed,
        )
        .unwrap_err();

    assert_eq!(
        err.required,
        WorkflowStage::Analyzed
    );

    assert_eq!(
        err.current,
        WorkflowStage::Discovered
    );
}

#[test]
fn full_pipeline_progression_reaches_exported(
) {
    let mut session =
        Session::new(
            "s1".to_string(),
        );

    session.complete_discovery(
        discovery_result(),
    );

    session.complete_validation(
        validation_result(),
    );

    session.complete_analysis(
        analysis_results(),
    );

    session.complete_export();

    assert_eq!(
        session.workflow,
        WorkflowStage::Exported
    );

    assert!(
        session
            .data
            .discovery
            .is_some()
    );

    assert!(
        session
            .data
            .validation
            .is_some()
    );

    assert!(
        session
            .data
            .analysis
            .is_some()
    );
}

/// Proves the real `Session` type — not a synthetic test fixture —
/// actually behaves correctly through the generic `InMemorySessionStore`
/// engine: its `HasSessionMetadata` impl must correctly expose the
/// session's real id, and a real save/get_handle/delete round trip
/// must work end to end. The store's own tests (in `unitprep-core`)
/// only prove the *mechanism* works against a fake session type; this
/// proves the actual wiring between the two is correct, which nothing
/// else specifically asserts.
#[test]
fn session_round_trips_through_generic_store(
) {
    let store: InMemorySessionStore<Session> =
        InMemorySessionStore::new();

    let session =
        Session::new("s1".to_string());

    store.save(session);

    let handle = store
        .get_handle("s1")
        .expect(
            "session should be retrievable immediately after save",
        );

    assert_eq!(
        handle.read().metadata().id,
        "s1"
    );

    store.delete("s1");

    assert!(
        store
            .get_handle("s1")
            .is_none(),
        "session should be gone after delete"
    );
}
