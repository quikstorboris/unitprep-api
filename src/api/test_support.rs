use std::sync::Arc;

use unitprep_core::in_memory_session_store::InMemorySessionStore;
use unitprep_core::session_store::SessionStore;
use unitprep_core::csv_document::CsvDocument;
use crate::application::dedup_session_service::DedupSession;
use crate::application::unit_group_session::Session;
use unitprep_unit_group::{
    AnalysisResults,
    BatchRun,
    DiscoveryResult,
    Severity,
    ValidationIssueSummary,
    ValidationResult,
};

use super::AppState;

/// Every test builder below needs a dedup session store too, even
/// though none of these UnitGroup-focused fixtures populate it —
/// `AppState` just needs the field present. Shared with
/// `dedup_test_support.rs`, which builds dedup sessions that actually
/// use one.
pub(crate) fn empty_dedup_store() -> Arc<dyn SessionStore<DedupSession>> {
    Arc::new(InMemorySessionStore::<DedupSession>::new())
}

// A pool that never actually connects -- connect_lazy only validates
// the URL is well-formed, so this is safe for every fixture below,
// none of which touch state.db yet. Add a real pool here only once a
// test actually needs to hit Postgres.
pub(crate) fn test_db_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://test:test@localhost/test")
        .expect("connect_lazy should never fail for a well-formed URL")
}

// A real WebauthnRsBackend, not a mock -- constructing one is pure
// computation (URL parsing + local struct setup), no I/O, so there is
// nothing to fake here. Safe for every fixture below, none of which
// exercise a real registration/authentication ceremony yet.
pub(crate) fn test_auth_backend() -> std::sync::Arc<dyn crate::auth::AuthBackend> {
    std::sync::Arc::new(
        crate::auth::WebauthnRsBackend::new("localhost", "http://localhost:3000")
            .expect("hardcoded localhost webauthn config should always be valid"),
    )
}

pub fn empty_state() -> AppState {
    AppState {
        unit_group_sessions: Arc::new(
            InMemorySessionStore::<Session>::new(),
        ),
        dedup_sessions: empty_dedup_store(),
        db: test_db_pool(),
        auth_backend: test_auth_backend(),
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
        modified_at: None,
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
        dedup_sessions: empty_dedup_store(),
        db: test_db_pool(),
        auth_backend: test_auth_backend(),
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

    let unit_file_names: Vec<String> = documents
        .iter()
        .map(|d| {
            d.file_name.clone()
        })
        .collect();

    session.data.documents =
        Arc::new(documents);

    session.complete_discovery(
        DiscoveryResult {
            unit_file_candidates:
                unit_file_names
                    .iter()
                    .map(|name| unitprep_unit_group::UnitFileCandidate {
                        file_name: name.clone(),
                        modified_at: None,
                        detected_vendor: "QSX".to_string(),
                    })
                    .collect(),
            selected_unit_file_name:
                unit_file_names.first().cloned(),
            requires_unit_file_selection: false,
            requires_format_resolution: false,
            detected_vendor_name:
                Some("QSX".to_string()),
            source_headers: Vec::new(),
            suggested_mapping: Vec::new(),
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
        dedup_sessions: empty_dedup_store(),
        db: test_db_pool(),
        auth_backend: test_auth_backend(),
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

    let unit_file_names: Vec<String> = documents
        .iter()
        .map(|d| {
            d.file_name.clone()
        })
        .collect();

    session.data.documents =
        Arc::new(documents);

    session.complete_discovery(
        DiscoveryResult {
            unit_file_candidates:
                unit_file_names
                    .iter()
                    .map(|name| unitprep_unit_group::UnitFileCandidate {
                        file_name: name.clone(),
                        modified_at: None,
                        detected_vendor: "QSX".to_string(),
                    })
                    .collect(),
            selected_unit_file_name:
                unit_file_names.first().cloned(),
            requires_unit_file_selection: false,
            requires_format_resolution: false,
            detected_vendor_name:
                Some("QSX".to_string()),
            source_headers: Vec::new(),
            suggested_mapping: Vec::new(),
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
            files_errored: Vec::new(),
            ready: true,
        },
    );

    let store = Arc::new(
        InMemorySessionStore::<Session>::new(),
    );

    store.save(session);

    AppState {
        unit_group_sessions: store,
        dedup_sessions: empty_dedup_store(),
        db: test_db_pool(),
        auth_backend: test_auth_backend(),
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

    let unit_file_names: Vec<String> = documents
        .iter()
        .map(|d| {
            d.file_name.clone()
        })
        .collect();

    session.data.documents =
        Arc::new(documents);

    session.complete_discovery(
        DiscoveryResult {
            unit_file_candidates:
                unit_file_names
                    .iter()
                    .map(|name| unitprep_unit_group::UnitFileCandidate {
                        file_name: name.clone(),
                        modified_at: None,
                        detected_vendor: "QSX".to_string(),
                    })
                    .collect(),
            selected_unit_file_name:
                unit_file_names.first().cloned(),
            requires_unit_file_selection: false,
            requires_format_resolution: false,
            detected_vendor_name:
                Some("QSX".to_string()),
            source_headers: Vec::new(),
            suggested_mapping: Vec::new(),
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
            files_errored: Vec::new(),
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
        dedup_sessions: empty_dedup_store(),
        db: test_db_pool(),
        auth_backend: test_auth_backend(),
    }
}
