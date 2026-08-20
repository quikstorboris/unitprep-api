//! `/discover` and the shared discovery-recomputation logic used by it,
//! `/unit-file/select`, and `/unit-file/resolve-format`. Split into
//! `dto` (request/response shapes), `compute` (the actual discovery
//! logic), `selection` (unit/group-file selection and readiness
//! resolution, plus discovered-group-name extraction), `format_helpers`
//! (header-shape/vendor-classification helpers also used outside this
//! module), and `format_resolution` (the confirm/manual-map resolution
//! logic behind `/unit-file/resolve-format`, which builds directly on
//! `format_helpers`) — separable concerns that had grown into one file
//! over time, not one idea.

mod compute;
mod dto;
mod format_helpers;
mod format_resolution;
mod selection;

pub use dto::DiscoverRequest;

pub(crate) use compute::compute_discovery;
pub(crate) use format_helpers::{
    current_unit_file_to_resolve, find_header_mismatches, is_group_document, normalized_headers,
};
pub(crate) use format_resolution::{resolve_confirm_action, validate_manual_mapping};

use std::time::Instant;

use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};

use unitprep_core::session_store::SessionStoreExt;

use crate::api::{session_not_found, AppState};
use crate::auth::AuthenticatedUser;

pub async fn discover(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Json(request): Json<DiscoverRequest>,
) -> Response {
    let started = Instant::now();

    // A synchronous read of the in-memory registry snapshot -- see
    // `client_ops::vendor_format`'s module doc comment for why this is
    // never a per-request DB call. Cloned up front (not read inside the
    // session-lock closure below) so the lock guard's lifetime never has
    // to interact with the session lock's own.
    let unit_vendors = state.unit_vendors.read().clone();

    let response = state.unit_group_sessions.with_owned_session_mut(
        &request.session_id,
        user.user_id,
        |session| {
            let response = compute_discovery(session, &unit_vendors);

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
        Some(response) => Json(response).into_response(),
        None => session_not_found(),
    }
}

#[cfg(test)]
#[path = "discover_tests.rs"]
mod tests;
