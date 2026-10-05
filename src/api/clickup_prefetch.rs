//! Warm-ups for ClickUp reads, so the answer is already cached when the
//! person needs it: the onboarding hierarchy (the Link ClickUp dialog) and
//! a facility's task list (the duplicate-check panel). Each endpoint
//! answers `202 Accepted` at once and does the ClickUp reading in a
//! background task.
//!
//! Safe to fire and forget because the work is read-only and repeatable:
//! if it fails, or nobody uses it, nothing is lost and a real request
//! simply loads the data itself. A real request that arrives while the
//! warm-up is still running waits for it (the caches are single-flight)
//! rather than repeating it. Writes are never done this way -- the user
//! needs to see how those went.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use uuid::Uuid;

use crate::api::clickup_connection::{clickup_client, load_user_token};
use crate::api::clickup_lookup::onboarding_space_name;
use crate::api::tool_runs::facility_belongs_to_company;
use crate::api::{internal_error, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clickup::{hierarchy, task_cache};

const PERMISSION: &str = "integrations.clickup";

/// Starts loading the onboarding hierarchy into the caller's cache.
pub async fn prefetch_hierarchy(
    State(state): State<AppState>,
    user: AuthenticatedUser,
) -> Response {
    if let Err(response) = user
        .require_permission(
            &state.db,
            PERMISSION,
            "prefetch_clickup_hierarchy",
            None,
            None,
        )
        .await
    {
        return response;
    }

    let (token, space_name) = match tokio::try_join!(
        load_user_token(&state, &user),
        onboarding_space_name(&state, &user)
    ) {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    let user_id = user.user_id;
    tokio::spawn(async move {
        let client = clickup_client(&state);
        if let Err(err) = hierarchy::cached_or_load(user_id, &client, &token, &space_name).await {
            tracing::debug!(error = %err, "ClickUp hierarchy prefetch failed");
        }
    });

    StatusCode::ACCEPTED.into_response()
}

/// Starts loading the task list of the facility's linked ClickUp list
/// into the caller's cache. Does nothing (still 202) for a facility that
/// is not linked.
pub async fn prefetch_facility_tasks(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "prefetch_clickup_tasks", None, None)
        .await
    {
        return response;
    }

    let list_id = match linked_list_id(&state, &user, company_id, facility_id).await {
        Ok(Some(list_id)) => list_id,
        Ok(None) => return StatusCode::ACCEPTED.into_response(),
        Err(response) => return response,
    };

    let token = match load_user_token(&state, &user).await {
        Ok(token) => token,
        Err(response) => return response,
    };

    let user_id = user.user_id;
    tokio::spawn(async move {
        let client = clickup_client(&state);
        let loaded = task_cache::get_or_load(user_id, &list_id, || async {
            client.list_tasks(&token, &list_id).await
        })
        .await;
        if let Err(err) = loaded {
            tracing::debug!(error = %err, "ClickUp task prefetch failed");
        }
    });

    StatusCode::ACCEPTED.into_response()
}

async fn linked_list_id(
    state: &AppState,
    user: &AuthenticatedUser,
    company_id: Uuid,
    facility_id: Uuid,
) -> Result<Option<String>, Response> {
    let fail = |err: sqlx::Error| {
        tracing::error!(error = %err, user_id = %user.user_id, "ClickUp prefetch lookup failed");
        internal_error("Could not prepare the ClickUp lookup")
    };

    let mut tx = begin_rls_transaction(&state.db, user.user_id, &user.role_keys)
        .await
        .map_err(fail)?;

    let list_id = if facility_belongs_to_company(&mut tx, facility_id, company_id)
        .await
        .map_err(fail)?
    {
        sqlx::query_scalar("SELECT clickup_list_id FROM clients.facilities WHERE id = $1")
            .bind(facility_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(fail)?
    } else {
        None
    };

    tx.commit().await.map_err(fail)?;
    Ok(list_id)
}
