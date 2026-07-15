use std::sync::Arc;

use unitprep_core::in_memory_session_store::InMemorySessionStore;
use unitprep_core::session_store::SessionStore;
use unitprep_core::csv_document::CsvDocument;
use crate::domain::models::{
    AnalysisResults,
    BatchRun,
    Severity,
};
use crate::domain::session::{
    DiscoveryResult,
    Session,
    ValidationIssueSummary,
    ValidationResult,
};

use super::AppState;

pub fn empty_state() -> AppState {
    AppState {
        unit_group_sessions: Arc::new(
            InMemorySessionStore::<Session>::new(),
        ),
    }
}

/// A minimal unit-file CsvDocument. `rows` are `[number, unitgroup,
/// width, length]` — enough to drive the dimension check, which is
/// what every endpoint under test here ultimately exercises.
pub fn unit_document(
    file_name: &str,
    rows: Vec<[&str; 4]>,
) -> CsvDocument {
    CsvDocument {
        file_name: file_name
            .to_string(),
        headers: vec![
            "number".to_string(),
            "unitgroup".to_string(),
            "width".to_string(),
            "length".to_string(),
        ],
        rows: rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|v| {
                        v.to_string()
                    })
                    .collect()
            })
            .collect(),
    }
}

/// A session holding `documents` but not yet discovered — what
/// `/discover` itself needs (it classifies documents on the fly, so
/// requires no particular stage going in).
pub fn uploaded_state(
    session_id: &str,
    documents: Vec<CsvDocument>,
) -> AppState {
    let mut session = Session::new(
        session_id.to_string(),
    );

    session.data.documents =
        Arc::new(documents);

    let store = Arc::new(
        InMemorySessionStore::<Session>::new(),
    );

    store.save(session);

    AppState {
        unit_group_sessions: store,
    }
}

/// A session past `Discovered`, with `documents` registered as unit
/// files — the minimum stage `/validate`, `/correct`, and
/// `/exempt-dimensions` need.
pub fn discovered_state(
    session_id: &str,
    documents: Vec<CsvDocument>,
) -> AppState {
    let mut session = Session::new(
        session_id.to_string(),
    );

    let unit_file_names = documents
        .iter()
        .map(|d| {
            d.file_name.clone()
        })
        .collect();

    session.data.documents =
        Arc::new(documents);

    session.complete_discovery(
        DiscoveryResult {
            unit_file_names,
            group_file_names:
                Vec::new(),
            selected_group_file_name:
                None,
            ready: true,
        },
    );

    let store = Arc::new(
        InMemorySessionStore::<Session>::new(),
    );

    store.save(session);

    AppState {
        unit_group_sessions: store,
    }
}

/// A session past `Validated` with no outstanding issues — what
/// `/analyze` needs to actually run instead of hitting its own
/// not-ready gate.
pub fn validated_state(
    session_id: &str,
    documents: Vec<CsvDocument>,
) -> AppState {
    let mut session = Session::new(
        session_id.to_string(),
    );

    let unit_file_names = documents
        .iter()
        .map(|d| {
            d.file_name.clone()
        })
        .collect();

    session.data.documents =
        Arc::new(documents);

    session.complete_discovery(
        DiscoveryResult {
            unit_file_names,
            group_file_names:
                Vec::new(),
            selected_group_file_name:
                None,
            ready: true,
        },
    );

    session.complete_validation(
        ValidationResult {
            files_checked: 1,
            issue_count: 0,
            error_count: 0,
            warning_count: 0,
            issues: Vec::new(),
            ready: true,
        },
    );

    let store = Arc::new(
        InMemorySessionStore::<Session>::new(),
    );

    store.save(session);

    AppState {
        unit_group_sessions: store,
    }
}

/// A session at `Analyzed` with one Error-severity validation issue
/// still outstanding and non-empty analysis results — what
/// `/export`'s acknowledge-override tests need: a session that's
/// legitimately blocked, not just missing.
pub fn analyzed_state_with_errors(
    session_id: &str,
    documents: Vec<CsvDocument>,
) -> AppState {
    let mut session = Session::new(
        session_id.to_string(),
    );

    let unit_file_names = documents
        .iter()
        .map(|d| {
            d.file_name.clone()
        })
        .collect();

    session.data.documents =
        Arc::new(documents);

    session.complete_discovery(
        DiscoveryResult {
            unit_file_names,
            group_file_names:
                Vec::new(),
            selected_group_file_name:
                None,
            ready: true,
        },
    );

    session.complete_validation(
        ValidationResult {
            files_checked: 1,
            issue_count: 1,
            error_count: 1,
            warning_count: 0,
            issues: vec![
                ValidationIssueSummary {
                    file_name: "units.csv".to_string(),
                    severity: Severity::Error,
                    description: "Invalid dimensions".to_string(),
                    affected_units: 1,
                    affected_unit_ids: vec!["A01".to_string()],
                    detail: "1 unit: A01".to_string(),
                    correctable_fields: vec!["width".to_string(), "length".to_string()],
                    exemptable: true,
                },
            ],
            ready: false,
        },
    );

    session.complete_analysis(
        AnalysisResults {
            batch_run: BatchRun {
                facilities:
                    Vec::new(),
                global_groups:
                    Default::default(),
                advisory_issues:
                    Vec::new(),
            },
            reference_groups: None,
            net_new_groups: vec![
                "10x10 Inside Climate".to_string(),
            ],
            similar_groups:
                Vec::new(),
        },
    );

    let store = Arc::new(
        InMemorySessionStore::<Session>::new(),
    );

    store.save(session);

    AppState {
        unit_group_sessions: store,
    }
}
