//! `/discover` and the shared discovery-recomputation logic used by it,
//! `/unit-file/select`, and `/unit-file/resolve-format`. Split into
//! `dto` (request/response shapes), `compute` (the actual discovery
//! logic), and `format_helpers` (header-shape/vendor-classification
//! helpers also used outside this module) — four separable concerns
//! that had grown into one 600+ line file over time, not one idea.

mod compute;
mod dto;
mod format_helpers;

pub use dto::DiscoverRequest;

pub(crate) use compute::compute_discovery;
pub(crate) use format_helpers::{
    current_unit_file_to_resolve,
    find_header_mismatches,
    is_group_document,
    normalized_headers,
};

use std::time::Instant;

use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};

use unitprep_core::session_store::SessionStoreExt;

use crate::api::{session_not_found, AppState};

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
                let response = compute_discovery(session);

                tracing::info!(
                    session_id = %request.session_id,
                    unit_files_found = response.unit_files_found,
                    group_files_found = response.group_files_found,
                    requires_unit_file_selection = response.requires_unit_file_selection,
                    requires_format_resolution = response.requires_format_resolution,
                    group_file_confirmed = response.group_file_confirmed,
                    ready = response.ready,
                    discovery_ms =
                        started
                            .elapsed()
                            .as_millis(),
                    "Discovery complete"
                );

                response
            },
        );

    match response {
        Some(response) => {
            Json(response).into_response()
        }
        None => session_not_found(),
    }
}

#[cfg(test)]
#[path = "discover_tests.rs"]
mod tests;
