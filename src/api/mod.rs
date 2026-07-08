mod analyze;
mod cancel_session;
mod correct;
mod discover;
mod exempt;
mod export;
mod select_group_file;
mod upload;
pub(crate) mod validate;

use std::sync::Arc;

use axum::{
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json,
    Router,
};

use serde::Serialize;

use tower_http::cors::{
    AllowOrigin,
    CorsLayer,
};

use crate::application::session_store::{
    SessionMetrics,
    SessionStore,
};

#[derive(Clone)]
pub struct AppState {
    pub session_store: Arc<dyn SessionStore>,
}

/// The one true "your session is gone" response — a session can disappear
/// either because it expired (10-minute idle timeout) or because the id
/// was never valid. Every endpoint that looks up a session by id should
/// return this instead of silently faking a zero-value success response,
/// so the frontend can distinguish "nothing to report" from "there's
/// nothing here to report on" and show an explicit expired-session screen
/// rather than a confusing all-zeros result.
pub(crate) fn session_not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        "Session not found or expired",
    )
        .into_response()
}

/// Origins allowed to call this API. Defaults to the frontend dev servers
/// so local development needs no configuration; set
/// `CORS_ALLOWED_ORIGINS` (comma-separated) to add real deployed
/// frontend origins instead of hardcoding them here.
fn allowed_origins() -> Vec<axum::http::HeaderValue> {
    match std::env::var("CORS_ALLOWED_ORIGINS")
    {
        Ok(value)
            if !value.trim().is_empty() =>
        {
            value
                .split(',')
                .map(|origin| {
                    origin.trim()
                })
                .filter(|origin| {
                    !origin.is_empty()
                })
                .filter_map(|origin| {
                    origin.parse().ok()
                })
                .collect()
        }

        _ => vec![
            "http://localhost:3000"
                .parse()
                .unwrap(),
            "http://localhost:5173"
                .parse()
                .unwrap(),
        ],
    }
}

pub fn router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(
            allowed_origins(),
        ))
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
        ]);

    Router::new()
        .route("/health", get(health))
        .route("/upload", post(upload::upload))
        .route("/discover", post(discover::discover))
        .route("/validate", post(validate::validate))
        .route("/correct", post(correct::correct))
        .route(
            "/exempt-dimensions",
            post(exempt::exempt_dimensions),
        )
        .route("/analyze", post(analyze::analyze))
        .route("/export", post(export::export))
        .route(
            "/group-file/select",
            post(select_group_file::select_group_file),
        )
        .route(
            "/session/cancel",
            post(cancel_session::cancel_session),
        )
        .layer(
            DefaultBodyLimit::max(
                100 * 1024 * 1024,
            ),
        )
        .with_state(state)
        .layer(cors)
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
    sessions: SessionMetrics,
}

async fn health(
    State(state): State<AppState>,
) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        // Read from Cargo.toml at compile time — bumping the version
        // there is the only thing needed to keep this in sync; nothing
        // to remember to update in two places.
        version: env!(
            "CARGO_PKG_VERSION"
        ),
        sessions: state
            .session_store
            .metrics(),
    })
}

/// Shared session-construction helpers for endpoint-level tests. Handlers
/// are called directly (`handler(State(state), Json(request)).await`)
/// rather than through a live HTTP router — `State`/`Json` are plain
/// public tuple structs, so this exercises the real handler logic
/// (session lookup, stage checks, response codes) without needing to
/// fabricate multipart bodies or spin up a server.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

    use crate::application::in_memory_session_store::InMemorySessionStore;
    use crate::application::session_store::SessionStore;
    use crate::domain::csv_document::CsvDocument;
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
            session_store: Arc::new(
                InMemorySessionStore::new(),
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
            InMemorySessionStore::new(),
        );

        store.save(session);

        AppState {
            session_store: store,
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
            InMemorySessionStore::new(),
        );

        store.save(session);

        AppState {
            session_store: store,
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
            InMemorySessionStore::new(),
        );

        store.save(session);

        AppState {
            session_store: store,
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
            InMemorySessionStore::new(),
        );

        store.save(session);

        AppState {
            session_store: store,
        }
    }
}