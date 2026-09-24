use super::*;
use unitprep_core::csv_document::CsvDocument;
use unitprep_core::in_memory_session_store::InMemorySessionStore;
use unitprep_core::session::HasSessionMetadata;
use unitprep_core::session_store::SessionStore;
use unitprep_unit_group::{AnalysisResults, BatchRun, ValidationResult};

fn document(file_name: &str, headers: Vec<&str>) -> CsvDocument {
    CsvDocument {
        modified_at: None,
        file_name: file_name.to_string(),
        headers: headers.into_iter().map(|h| h.to_string()).collect(),
        rows: Vec::new(),
    }
}

/// DoorSwap's real signature/mapping, hand-built to mirror the
/// `client_ops.vendor_format` registry migration's seed row -- vendor
/// recognition is DB-backed data now, so `effective_documents`'s own
/// auto-detect fallback (see `SessionData::unit_vendors`) needs a
/// fixture list here the same way a live session would carry a snapshot
/// taken by `compute_discovery`.
fn door_swap_vendor() -> unitprep_core::vendor_format::VendorFormat {
    unitprep_core::vendor_format::VendorFormat {
        name: "DoorSwap".to_string(),
        content_type: unitprep_core::vendor_format::ContentType::Units,
        signature_headers: vec!["Unit", "Unit Type", "Status", "Customer"]
            .into_iter()
            .map(String::from)
            .collect(),
        field_mapping: vec![
            ("Number", "Unit"),
            ("UnitGroup", "Unit Type"),
            ("Status", "Status"),
            ("Customer", "Customer"),
        ]
        .into_iter()
        .map(|(t, s)| (t.to_string(), s.to_string()))
        .collect(),
        transform_key: None,
    }
}

fn discovery_result() -> DiscoveryResult {
    DiscoveryResult {
        unit_file_names: vec!["units.csv".to_string()],
        group_file_names: vec!["groups.csv".to_string()],
        selected_group_file_name: Some("groups.csv".to_string()),
        ready: true,
        unit_file_candidates: vec![unitprep_unit_group::UnitFileCandidate {
            file_name: "units.csv".to_string(),
            modified_at: None,
            detected_vendor: "QSX".to_string(),
        }],
        selected_unit_file_names: vec!["units.csv".to_string()],
        requires_unit_file_selection: false,
        requires_format_resolution: false,
        current_unit_file_name: None,
        pending_unit_file_names: Vec::new(),
        detected_vendor_name: Some("QSX".to_string()),
        source_headers: Vec::new(),
        suggested_mapping: Vec::new(),
    }
}

fn validation_result() -> ValidationResult {
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

fn analysis_results() -> AnalysisResults {
    AnalysisResults {
        batch_run: BatchRun {
            facilities: Vec::new(),
            global_groups: Default::default(),
            advisory_issues: Vec::new(),
        },
        reference_groups: None,
        net_new_groups: Vec::new(),
        similar_groups: Vec::new(),
    }
}

#[test]
fn new_session_starts_uploaded() {
    let session = Session::new("s1".to_string(), None);

    assert_eq!(session.workflow, WorkflowStage::Uploaded);

    assert!(session.require_stage(WorkflowStage::Uploaded).is_ok());

    assert!(session.require_stage(WorkflowStage::Discovered).is_err());
}

#[test]
fn new_session_starts_at_generation_zero() {
    let session = Session::new("s1".to_string(), None);

    assert_eq!(session.data_generation(), 0);
}

/// Every mutation that can change what `effective_documents` produces
/// must bump `data_generation` -- this is what `/analyze` and `/export`
/// compare against their own captured read-time value to detect a
/// concurrent correction landing in their read -> write-back gap (see
/// analyze_tests.rs/export_tests.rs's TOCTOU regression tests).
#[test]
fn every_data_mutating_method_bumps_the_generation() {
    let mut session = Session::new("s1".to_string(), None);

    let mut previous = session.data_generation();

    session.add_correction(
        unitprep_unit_group::CorrectionKey {
            file_name: "units.csv".to_string(),
            unit_number: "A01".to_string(),
            field: "width".to_string(),
        },
        "10".to_string(),
    );
    assert!(session.data_generation() > previous);
    previous = session.data_generation();

    session.add_dimension_exemption(unitprep_unit_group::DimensionExemptionKey {
        file_name: "units.csv".to_string(),
        unit_number: "Office".to_string(),
    });
    assert!(session.data_generation() > previous);
    previous = session.data_generation();

    session.exclude_group("10x10 Inside Climate".to_string());
    assert!(session.data_generation() > previous);
    previous = session.data_generation();

    session.include_group("10x10 Inside Climate");
    assert!(session.data_generation() > previous);
    previous = session.data_generation();

    session.acknowledge_group_check("Odd UnitGroup values".to_string(), "Office".to_string());
    assert!(session.data_generation() > previous);
    previous = session.data_generation();

    session.unacknowledge_group_check("Odd UnitGroup values", "Office");
    assert!(session.data_generation() > previous);
    previous = session.data_generation();

    session.upsert_document(document("units.csv", vec!["number", "unitgroup"]));
    assert!(session.data_generation() > previous);
}

/// `complete_validation`/`complete_analysis`/`complete_export` are driven
/// by the results of a validation/analysis run, not a direct data edit,
/// so they deliberately do NOT bump the generation on their own.
/// `complete_discovery` is different -- see its own doc comment and the
/// `complete_discovery_bumps_the_generation` test below.
#[test]
fn completing_downstream_stages_does_not_bump_the_generation_on_its_own() {
    let mut session = Session::new("s1".to_string(), None);

    session.complete_discovery(discovery_result());
    let after_discovery = session.data_generation();

    session.complete_validation(validation_result());
    assert_eq!(session.data_generation(), after_discovery);

    session.complete_analysis(Arc::new(analysis_results()));
    assert_eq!(session.data_generation(), after_discovery);

    session.complete_export();
    assert_eq!(session.data_generation(), after_discovery);
}

/// `complete_discovery` is the single funnel every discovery-affecting
/// handler goes through (`/discover`, `/unit-file/select`,
/// `/unit-file/resolve-format`, `/group-file/select`,
/// `/group-file/confirm`, `/group-file/upload`) -- including handlers
/// that mutate `SessionData` fields (`format_resolutions`,
/// `selected_group_file_name`, `group_file_confirmed`) directly rather
/// than through a `touch_data`-calling method. It must bump
/// `data_generation` itself so those mutations are caught by the same
/// analyze/export TOCTOU guard as `add_correction` et al. -- see the
/// field's own doc comment.
#[test]
fn complete_discovery_bumps_the_generation() {
    let mut session = Session::new("s1".to_string(), None);

    let previous = session.data_generation();

    session.complete_discovery(discovery_result());

    assert!(session.data_generation() > previous);
}

#[test]
fn stage_ordering_is_pipeline_order() {
    assert!(WorkflowStage::Uploaded < WorkflowStage::Discovered);

    assert!(WorkflowStage::Discovered < WorkflowStage::Validated);

    assert!(WorkflowStage::Validated < WorkflowStage::Analyzed);

    assert!(WorkflowStage::Analyzed < WorkflowStage::Exported);
}

#[test]
fn complete_discovery_advances_stage_and_stores_data() {
    let mut session = Session::new("s1".to_string(), None);

    session.complete_discovery(discovery_result());

    assert_eq!(session.workflow, WorkflowStage::Discovered);

    assert!(session.data.discovery.is_some());
}

#[test]
fn require_stage_reports_current_stage_on_failure() {
    let mut session = Session::new("s1".to_string(), None);

    session.complete_discovery(discovery_result());

    let err = session.require_stage(WorkflowStage::Analyzed).unwrap_err();

    assert_eq!(err.required, WorkflowStage::Analyzed);

    assert_eq!(err.current, WorkflowStage::Discovered);
}

#[test]
fn full_pipeline_progression_reaches_exported() {
    let mut session = Session::new("s1".to_string(), None);

    session.complete_discovery(discovery_result());

    session.complete_validation(validation_result());

    session.complete_analysis(Arc::new(analysis_results()));

    session.complete_export();

    assert_eq!(session.workflow, WorkflowStage::Exported);

    assert!(session.data.discovery.is_some());

    assert!(session.data.validation.is_some());

    assert!(session.data.analysis.is_some());
}

#[test]
fn upsert_document_appends_a_new_file() {
    let mut session = Session::new("s1".to_string(), None);

    session.upsert_document(document("a.csv", vec!["number"]));

    assert_eq!(session.data.documents.len(), 1);

    assert_eq!(session.data.documents[0].file_name, "a.csv");
}

#[test]
fn upsert_document_replaces_an_existing_file_by_name() {
    let mut session = Session::new("s1".to_string(), None);

    session.upsert_document(document("a.csv", vec!["number"]));

    session.upsert_document(document("a.csv", vec!["number", "unitgroup"]));

    assert_eq!(
        session.data.documents.len(),
        1,
        "should replace, not duplicate"
    );

    assert_eq!(
        session.data.documents[0].headers,
        vec!["number".to_string(), "unitgroup".to_string()]
    );
}

#[test]
fn upsert_document_leaves_other_documents_untouched() {
    let mut session = Session::new("s1".to_string(), None);

    session.upsert_document(document("a.csv", vec!["number"]));

    session.upsert_document(document("b.csv", vec!["unitgroup"]));

    assert_eq!(session.data.documents.len(), 2);
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
fn session_round_trips_through_generic_store() {
    let store: InMemorySessionStore<Session> = InMemorySessionStore::new();

    let session = Session::new("s1".to_string(), None);

    store.save(session);

    let handle = store
        .get_handle("s1")
        .expect("session should be retrievable immediately after save");

    assert_eq!(handle.read().metadata().id, "s1");

    store.delete("s1");

    assert!(
        store.get_handle("s1").is_none(),
        "session should be gone after delete"
    );
}

#[test]
fn effective_documents_auto_detects_vendor_when_no_stored_resolution_exists() {
    let mut session = Session::new("s1".to_string(), None);

    // DoorSwap's real signature headers -- no format_resolutions entry
    // is stored for this file, so the fallback must auto-detect the
    // vendor and map it into canonical columns itself, rather than
    // passing the raw DoorSwap headers through unmapped.
    session.upsert_document(document(
        "units.csv",
        vec!["Unit", "Unit Type", "Status", "Customer"],
    ));
    session.data.unit_vendors = vec![door_swap_vendor()];

    let effective = session.effective_documents();

    assert_eq!(effective.len(), 1);
    assert_eq!(
        effective[0].headers,
        vec![
            "Number".to_string(),
            "UnitGroup".to_string(),
            "Status".to_string(),
            "Customer".to_string(),
        ]
    );
}

#[test]
fn effective_documents_prefers_a_stored_resolution_over_auto_detection() {
    let mut session = Session::new("s1".to_string(), None);

    session.upsert_document(document(
        "units.csv",
        vec!["Unit", "Unit Type", "Status", "Customer"],
    ));

    // A manual mapping stored for this file should win over
    // auto-detection, even though the file also happens to match
    // DoorSwap's signature.
    session.data.format_resolutions.insert(
        "units.csv".to_string(),
        vec![("Number".to_string(), Some("Customer".to_string()))],
    );

    let effective = session.effective_documents();

    assert_eq!(effective.len(), 1);
    assert_eq!(effective[0].headers, vec!["Number".to_string()]);
}

/// Integration test for Group Prep's `Session` surviving a process
/// restart via `DurableSessionStore<Session>` -- the same real-DB
/// pattern `registration_ceremony.rs`'s own
/// `a_registration_ceremony_survives_a_simulated_process_restart_durability`
/// test uses (see that test's doc comment for the full rationale: this
/// project verifies durability empirically against a real database, not
/// just by reading the `#[derive(Serialize, Deserialize)]` list and
/// assuming it round-trips).
///
/// Unlike the WebAuthn ceremony, `Session` is a much larger struct with
/// real nested collections (documents, discovery/validation/analysis
/// results, corrections, exemptions, acknowledgments, a vendor
/// snapshot) -- the point of building a reasonably full one here, not a
/// bare `Session::new`, is to prove every one of those fields actually
/// makes it through a real bincode round trip via Postgres, not just
/// the struct's shallow-empty-default shape.
///
/// `metadata.owner_id` is left `None` for the same reason the WebAuthn
/// test leaves it `None` -- see that test's own doc comment (no
/// `auth.users` DELETE policy exists to clean up a throwaway row
/// afterward, and the column's value is opaque to durability itself).
///
/// Run explicitly with `cargo test -- --ignored durability`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
async fn a_group_prep_session_survives_a_simulated_process_restart_durability() {
    use std::collections::{HashMap, HashSet};
    use std::time::Duration;

    use unitprep_core::durable_session_store::DurableSessionStore;
    use unitprep_core::session_store::SessionStore;

    let _ = dotenvy::from_filename(".env.local");

    let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");

    // A unique kind per test run -- keeps this test's row fully isolated
    // from anything a concurrently-running real server instance (or a
    // concurrently-running copy of this very test) might be persisting
    // under the real "unit_group_session" kind main.rs will use.
    let kind = format!("test_unit_group_session_{}", Uuid::new_v4());
    let session_id = format!("test-session-{}", Uuid::new_v4());

    let mut session = Session::new(session_id.clone(), None);

    // Two real, differently-shaped documents -- a unit file with actual
    // rows and a group (master) file -- rather than the empty-rows
    // `document()` fixture the other tests above use, since the point
    // here is proving real row data round-trips, not just headers.
    session.data.documents = Arc::new(vec![
        CsvDocument {
            file_name: "units.csv".to_string(),
            headers: vec![
                "Number".to_string(),
                "UnitGroup".to_string(),
                "Width".to_string(),
            ],
            rows: vec![
                vec![
                    "A01".to_string(),
                    "10x10 Inside Climate".to_string(),
                    "10".to_string(),
                ],
                vec![
                    "A02".to_string(),
                    "10x10 Inside Climate".to_string(),
                    "10".to_string(),
                ],
                vec![
                    "B01".to_string(),
                    "10x20 Outside".to_string(),
                    "10".to_string(),
                ],
            ],
            modified_at: Some(1_700_000_000_000),
        },
        CsvDocument {
            file_name: "groups.csv".to_string(),
            headers: vec!["Name".to_string()],
            rows: vec![
                vec!["10x10 Inside Climate".to_string()],
                vec!["10x20 Outside".to_string()],
            ],
            modified_at: None,
        },
    ]);

    session.data.unit_vendors = vec![door_swap_vendor()];

    session.complete_discovery(discovery_result());
    session.complete_validation(validation_result());
    session.complete_analysis(Arc::new(analysis_results()));

    session.add_correction(
        unitprep_unit_group::CorrectionKey {
            file_name: "units.csv".to_string(),
            unit_number: "A01".to_string(),
            field: "width".to_string(),
        },
        "12".to_string(),
    );

    session.add_dimension_exemption(unitprep_unit_group::DimensionExemptionKey {
        file_name: "units.csv".to_string(),
        unit_number: "B01".to_string(),
    });

    session.data.format_resolutions.insert(
        "units.csv".to_string(),
        vec![
            ("Number".to_string(), Some("Number".to_string())),
            ("UnitGroup".to_string(), Some("UnitGroup".to_string())),
        ],
    );

    session.data.group_file_confirmed = true;

    session.exclude_group("Excluded Group".to_string());

    session.acknowledge_group_check("Odd UnitGroup values".to_string(), "Office".to_string());

    session.data.source_dropbox_folder_path = Some("/Facilities/Test Facility".to_string());

    // Snapshot everything before it moves into the store, to compare
    // against what comes back out after the simulated restart.
    let original_workflow = session.workflow;
    let original_data_generation = session.data_generation();
    let original_documents: Vec<CsvDocument> = session.data.documents.as_ref().clone();
    let original_discovery = session.data.discovery.clone();
    let original_validation = session.data.validation.clone();
    let original_analysis = session.data.analysis.clone();
    let original_corrections: HashMap<_, _> = session.data.corrections.clone();
    let original_dimension_exemptions: HashSet<_> = session.data.dimension_exemptions.clone();
    let original_format_resolutions = session.data.format_resolutions.clone();
    let original_group_file_confirmed = session.data.group_file_confirmed;
    let original_excluded_groups: HashSet<_> = session.data.excluded_groups.clone();
    let original_group_check_acknowledgments: HashSet<_> =
        session.data.group_check_acknowledgments.clone();
    let original_unit_vendors_len = session.data.unit_vendors.len();
    let original_source_dropbox_folder_path = session.data.source_dropbox_folder_path.clone();

    let store_before_restart = DurableSessionStore::<Session>::with_timeout(
        db.clone(),
        kind.clone(),
        Duration::from_secs(5 * 60),
    );

    store_before_restart.save(session);

    // save()'s Postgres write is fire-and-forget (see
    // DurableSessionStore::persist's own doc comment) -- poll briefly
    // for the row to actually land before simulating a restart, same as
    // the WebAuthn ceremony's own version of this test.
    let mut persisted = false;

    for _ in 0..20 {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM auth.durable_sessions WHERE kind = $1 AND id = $2",
        )
        .bind(&kind)
        .bind(&session_id)
        .fetch_one(&db)
        .await
        .expect("querying auth.durable_sessions must not fail");

        if count == 1 {
            persisted = true;
            break;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    assert!(
        persisted,
        "save() must write a row to auth.durable_sessions within ~2 seconds"
    );

    // Simulate a process restart: drop the store entirely (its
    // in-memory layer, and every handle into it, goes with it) and
    // build a brand new one against the SAME underlying Postgres
    // connection pool.
    drop(store_before_restart);

    let store_after_restart = DurableSessionStore::<Session>::with_timeout(
        db.clone(),
        kind.clone(),
        Duration::from_secs(5 * 60),
    );

    let handle = store_after_restart
        .get_handle(&session_id)
        .expect("get_handle must rehydrate the session from Postgres after a simulated restart");

    {
        let rehydrated = handle.read();

        assert_eq!(rehydrated.metadata.id, session_id);
        assert_eq!(rehydrated.metadata.owner_id, None);
        assert!(!rehydrated.metadata.cancelled);

        assert_eq!(rehydrated.workflow, original_workflow);
        assert_eq!(rehydrated.data_generation(), original_data_generation);

        assert_eq!(
            rehydrated.data.documents.as_ref().clone(),
            original_documents
        );
        assert_eq!(rehydrated.data.discovery, original_discovery);
        assert_eq!(rehydrated.data.validation, original_validation);
        assert_eq!(
            rehydrated
                .data
                .analysis
                .as_ref()
                .map(|a| a.as_ref().clone()),
            original_analysis.as_ref().map(|a| a.as_ref().clone())
        );
        assert_eq!(rehydrated.data.corrections, original_corrections);
        assert_eq!(
            rehydrated.data.dimension_exemptions,
            original_dimension_exemptions
        );
        assert_eq!(
            rehydrated.data.format_resolutions,
            original_format_resolutions
        );
        assert_eq!(
            rehydrated.data.group_file_confirmed,
            original_group_file_confirmed
        );
        assert_eq!(rehydrated.data.excluded_groups, original_excluded_groups);
        assert_eq!(
            rehydrated.data.group_check_acknowledgments,
            original_group_check_acknowledgments
        );
        assert_eq!(
            rehydrated.data.unit_vendors.len(),
            original_unit_vendors_len
        );
        assert_eq!(rehydrated.data.unit_vendors[0].name, "DoorSwap");
        assert_eq!(
            rehydrated.data.source_dropbox_folder_path,
            original_source_dropbox_folder_path
        );
    }

    // Clean up -- leave no row behind for the next run.
    store_after_restart.delete(&session_id);

    tokio::time::sleep(Duration::from_millis(300)).await;
}
