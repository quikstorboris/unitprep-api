//! Manual trigger + progress polling for the Process Street
//! person-index sync (`clients::sync`) -- the "Sync Now" button on the
//! search page, and the progress bar it drives. Shares `AppState::
//! sync_progress` with the nightly background task (`main.rs`'s
//! `start_background_sync_task` call) via `try_claim_running`, so a
//! click landing mid-nightly-run (or vice versa) just reports "already
//! running" instead of starting a second, overlapping pass.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

use crate::api::{conflict, process_street_not_configured, user_agent_from, AppState};
use crate::auth::AuthenticatedUser;
use crate::clients::sync::{run_all_workflows_with_progress, try_claim_running, SyncState};

const PERMISSION: &str = "client_ops.perform";

fn already_running() -> Response {
    conflict(
        "sync_already_running",
        "A Process Street sync is already running.".to_string(),
    )
}

#[derive(Debug, Serialize)]
pub struct StartSyncResponse {
    started: bool,
}

/// `?force=true` -- see `sync::orchestrator::sync_workflow_runs`'s own
/// doc comment for exactly what this changes (every run in every
/// workflow is treated as never-synced-before, not just the ones PS
/// itself says changed) and why it costs real Process Street API
/// budget to run. Defaults to `false` so the existing "Sync Now" button
/// (which sends no query string at all) keeps its current, cheap
/// delta-only behavior unchanged.
#[derive(Debug, Default, Deserialize)]
pub struct StartSyncQuery {
    #[serde(default)]
    pub force: bool,
}

/// Requires `client_ops.perform` -- same standing permission every
/// other client-data-mutating action in this app gates on
/// (`onboarding_manager`/`department_manager` both hold it); the actual
/// writes always run under the sync's own fixed system role regardless
/// of who triggers it (see `clients::sync`'s own RLS reasoning), so this
/// gate is about who may kick off the work, not a data-access boundary.
pub async fn start_sync(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Query(query): Query<StartSyncQuery>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    if let Err(response) = user
        .require_permission(
            &state.db,
            PERMISSION,
            "start_process_street_sync",
            user_agent,
            None,
        )
        .await
    {
        return response;
    }

    let Some(client) = state.process_street.clone() else {
        return process_street_not_configured();
    };

    if !try_claim_running(&state.sync_progress) {
        return already_running();
    }

    tracing::info!(
        user_id = %user.user_id,
        force = query.force,
        "user triggered a manual Process Street sync"
    );

    let db = state.db.clone();
    let progress = state.sync_progress.clone();
    let actor_user_id = user.user_id;
    let force = query.force;
    tokio::spawn(async move {
        run_all_workflows_with_progress(&client, &db, &progress, actor_user_id, force).await;
    });

    (
        StatusCode::ACCEPTED,
        Json(StartSyncResponse { started: true }),
    )
        .into_response()
}

#[derive(Debug, Serialize)]
pub struct SyncStatusResponse {
    /// `idle` | `running` | `completed` | `failed`.
    state: &'static str,
    total_runs: usize,
    processed_runs: usize,
    percent: u8,
    error: Option<String>,
}

/// Any authenticated caller, no permission gate -- a progress readout,
/// not an action. Never touches Postgres/PS itself, just reads the
/// in-memory handle, so this is safe to poll frequently.
pub async fn sync_status(State(state): State<AppState>, _user: AuthenticatedUser) -> Response {
    if state.process_street.is_none() {
        return process_street_not_configured();
    }

    let progress = state.sync_progress.read();

    let state_label = match progress.state {
        SyncState::Idle => "idle",
        SyncState::Running => "running",
        SyncState::Completed => "completed",
        SyncState::Failed => "failed",
    };

    Json(SyncStatusResponse {
        state: state_label,
        total_runs: progress.total_runs,
        processed_runs: progress.processed_runs,
        percent: progress.percent(),
        error: progress.error.clone(),
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, onboarding_manager_user, test_user};

    #[tokio::test]
    async fn start_sync_refuses_insufficient_permission_without_touching_anything() {
        let response = start_sync(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Query(StartSyncQuery::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn start_sync_reports_not_configured_with_sufficient_permission() {
        // empty_state() carries process_street: None -- confirms a
        // permitted caller still gets a clear 503, not a panic or a
        // silently-accepted no-op, when PS isn't configured.
        let response = start_sync(
            State(empty_state()),
            onboarding_manager_user(),
            HeaderMap::new(),
            Query(StartSyncQuery::default()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn sync_status_reports_not_configured_when_process_street_is_none() {
        let response = sync_status(State(empty_state()), test_user()).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
