use std::time::Instant;

use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use unitprep_core::session_store::SessionStoreExt;

use crate::{
    api::{session_not_found, AppState},
    domain::session::DiscoveryResult,
};

#[derive(Debug, Deserialize)]
pub struct DiscoverRequest {
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct DiscoverResponse {
    pub unit_files_found: usize,
    pub group_files_found: usize,
    pub group_file_names: Vec<String>,
    pub selected_group_file_name:
        Option<String>,
    pub requires_group_selection:
        bool,
    pub ready: bool,
}

pub async fn discover(
    State(state): State<AppState>,
    Json(request): Json<DiscoverRequest>,
) -> Response {
    let started = Instant::now();

    let response = state
        .unit_group_sessions
        .with_session_mut(
            &request.session_id,
            |session| {
                let mut unit_files =
                    Vec::new();

                let mut group_files =
                    Vec::new();

                let mut unrecognized_count =
                    0usize;

                for document in
                    session.data.documents.iter()
                {
                    if is_unit_document(document) {
                        unit_files.push(
                            document
                                .file_name
                                .clone(),
                        );
                    } else if is_group_document(document) {
                        group_files.push(
                            document
                                .file_name
                                .clone(),
                        );
                    } else {
                        unrecognized_count += 1;
                    }
                }

                let selected_group_file_name =
                    if group_files.len() == 1 {
                        Some(
                            group_files[0]
                                .clone(),
                        )
                    } else {
                        None
                    };

                let ready =
                    !unit_files.is_empty()
                        && !group_files.is_empty()
                        && group_files.len()
                            == 1;

                let discovery =
                    DiscoveryResult {
                        unit_file_names:
                            unit_files.clone(),
                        group_file_names:
                            group_files.clone(),
                        selected_group_file_name:
                            selected_group_file_name
                                .clone(),
                        ready,
                    };

                session.complete_discovery(
                    discovery.clone(),
                );

                tracing::info!(
                    session_id = %request.session_id,
                    unit_files_found =
                        discovery
                            .unit_file_names
                            .len(),
                    group_files_found =
                        discovery
                            .group_file_names
                            .len(),
                    unrecognized_files =
                        unrecognized_count,
                    requires_group_selection =
                        discovery
                            .group_file_names
                            .len()
                            > 1,
                    ready =
                        discovery.ready,
                    discovery_ms =
                        started
                            .elapsed()
                            .as_millis(),
                    "Discovery complete"
                );

                DiscoverResponse {
                    unit_files_found:
                        discovery
                            .unit_file_names
                            .len(),
                    group_files_found:
                        discovery
                            .group_file_names
                            .len(),
                    group_file_names:
                        discovery
                            .group_file_names
                            .clone(),
                    selected_group_file_name:
                        discovery
                            .selected_group_file_name
                            .clone(),
                    requires_group_selection:
                        discovery
                            .group_file_names
                            .len()
                            > 1,
                    ready:
                        discovery.ready,
                }
            },
        );

    match response {
        Some(response) => {
            Json(response).into_response()
        }
        None => session_not_found(),
    }
}

// Column presence is decided through `CsvDocument::header_index` — the
// single normalization rule every lookup in the system shares (see its
// doc comment) — rather than each caller building its own normalized
// header list. That's deliberate: this file previously had its own
// `normalize()` that stripped spaces/underscores while `header_index`
// only lowercased, so a header like "Unit_Group" could pass this check
// and then silently fail every subsequent lookup validation did.

fn is_unit_document(
    document: &unitprep_core::csv_document::CsvDocument,
) -> bool {
    document
        .header_index("unitgroup")
        .is_some()
        && document
            .header_index("number")
            .is_some()
        && document
            .header_index("category")
            .is_some()
}

fn is_group_document(
    document: &unitprep_core::csv_document::CsvDocument,
) -> bool {
    let required = [
        "name",
        "description",
        "assignedto",
        "status",
        "lastupdated",
    ];

    required.iter().all(|r| {
        document
            .header_index(r)
            .is_some()
    })
}


#[cfg(test)]
#[path = "discover_tests.rs"]
mod tests;
