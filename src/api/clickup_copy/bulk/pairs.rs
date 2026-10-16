//! `bulk-pairs`: each destination's suggested counterpart of the chosen
//! source task.

use axum::{
    extract::{Json, Path, Query, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::super::lists::{load_tasks_lenient, parse_scope, valid_task_id, PERMISSION};
use super::super::pairs::{target_choice, task_info, TargetChoice, TaskInfo};
use super::context::prepare_bulk;
use crate::api::clickup_connection::load_user_token;
use crate::api::rls::try_response;
use crate::api::{bad_request, conflict, AppState};
use crate::auth::AuthenticatedUser;
use crate::clickup::copy_pairing;
use crate::clickup::task_matching;

/// Destination lists read at once.
const LIST_READ_CONCURRENCY: usize = 4;

#[derive(Debug, Deserialize)]
pub struct BulkPairsQuery {
    pub source_facility_id: Option<Uuid>,
    pub source_task_id: String,
    pub scope: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DestinationPairs {
    pub facility_id: Uuid,
    pub facility_name: String,
    pub list_name: String,
    pub list_url: String,
    /// The suggested counterpart of the source task; `None` is "no match".
    pub target: Option<TargetChoice>,
    pub alternatives: Vec<TargetChoice>,
    /// Every eligible task in this facility's list, to choose by hand.
    pub tasks: Vec<TaskInfo>,
    /// Why this facility's list could not be read, when it could not.
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BulkPairsResponse {
    pub destinations: Vec<DestinationPairs>,
}

/// `GET /clients/{company_id}/clickup/bulk-pairs`
pub async fn bulk_pairs(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
    Query(query): Query<BulkPairsQuery>,
) -> Response {
    try_response!(
        user.require_permission(&state.db, PERMISSION, "clickup_bulk_pairs", None, None)
            .await
    );
    let source_task_id = query.source_task_id.trim().to_string();
    if !valid_task_id(&source_task_id) {
        return bad_request("invalid_clickup_task", "Choose a ClickUp task.".to_string());
    }
    let scope = match parse_scope(query.scope.as_deref()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    let (ctx, token) = match tokio::try_join!(
        prepare_bulk(&state, &user, company_id, query.source_facility_id),
        load_user_token(&state, &user)
    ) {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    let source_tasks = match load_tasks_lenient(&state, &user, &token, &ctx.source).await {
        Ok(tasks) => tasks,
        Err(message) => return conflict("clickup_list_unavailable", message),
    };
    if !source_tasks.iter().any(|task| task.id == source_task_id) {
        return bad_request(
            "task_not_in_list",
            "That task is not in the source facility's ClickUp list.".to_string(),
        );
    }

    // A few lists at a time; each is cached for later requests.
    let mut destinations = Vec::with_capacity(ctx.destinations.len());
    for chunk in ctx.destinations.chunks(LIST_READ_CONCURRENCY) {
        let loaded = futures::future::join_all(
            chunk
                .iter()
                .map(|list| load_tasks_lenient(&state, &user, &token, list)),
        )
        .await;

        for (list, loaded) in chunk.iter().zip(loaded) {
            let mut entry = DestinationPairs {
                facility_id: list.facility_id,
                facility_name: list.facility_name.clone(),
                list_name: list.list_name.clone(),
                list_url: list.list_url.clone(),
                target: None,
                alternatives: Vec::new(),
                tasks: Vec::new(),
                error: None,
            };

            match loaded {
                Err(message) => entry.error = Some(message),
                Ok(tasks) => {
                    let parents = task_matching::parent_names(&tasks);
                    if let Some(pairing) = copy_pairing::pair(&source_tasks, &tasks, scope)
                        .into_iter()
                        .find(|pairing| pairing.source.id == source_task_id)
                    {
                        entry.target = pairing.target.as_ref().map(|c| target_choice(c, &parents));
                        entry.alternatives = pairing
                            .alternatives
                            .iter()
                            .map(|c| target_choice(c, &parents))
                            .collect();
                    }
                    entry.tasks = copy_pairing::eligible(&tasks, scope)
                        .into_iter()
                        .map(|task| task_info(task, &parents))
                        .collect();
                }
            }
            destinations.push(entry);
        }
    }

    Json(BulkPairsResponse { destinations }).into_response()
}
