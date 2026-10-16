//! `bulk-tasks`: the source list's tasks and the possible destinations.

use axum::{
    extract::{Json, Path, Query, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::super::lists::{load_tasks_lenient, parse_scope, FacilityList, PERMISSION};
use super::super::pairs::{task_info, TaskInfo};
use super::context::prepare_bulk;
use crate::api::clickup_connection::load_user_token;
use crate::api::rls::try_response;
use crate::api::{conflict, AppState};
use crate::auth::AuthenticatedUser;
use crate::clickup::copy_pairing;
use crate::clickup::task_matching;

#[derive(Debug, Deserialize)]
pub struct BulkTasksQuery {
    pub source_facility_id: Option<Uuid>,
    /// `corporate`, `facility` or `all` (the default).
    pub scope: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FacilityRef {
    pub facility_id: Uuid,
    pub facility_name: String,
}

#[derive(Debug, Serialize)]
pub struct BulkTask {
    pub phase: String,
    /// The phase's position in the template's own order.
    pub phase_order: i64,
    #[serde(flatten)]
    pub task: TaskInfo,
}

#[derive(Debug, Serialize)]
pub struct BulkTasksResponse {
    pub source: FacilityList,
    /// The company's main list, named by the pointer comment.
    pub parent: Option<FacilityList>,
    pub tasks: Vec<BulkTask>,
    /// Facilities the comment can be copied to.
    pub destinations: Vec<FacilityRef>,
    /// Facilities with no ClickUp list linked yet, so not offered.
    pub unlinked: Vec<FacilityRef>,
}

/// `GET /clients/{company_id}/clickup/bulk-tasks`
pub async fn bulk_tasks(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
    Query(query): Query<BulkTasksQuery>,
) -> Response {
    try_response!(
        user.require_permission(&state.db, PERMISSION, "clickup_bulk_tasks", None, None)
            .await
    );
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
    let parents = task_matching::parent_names(&source_tasks);

    let tasks = copy_pairing::eligible(&source_tasks, scope)
        .into_iter()
        .map(|task| BulkTask {
            phase: copy_pairing::copy_phase(task)
                .unwrap_or_default()
                .to_string(),
            phase_order: copy_pairing::phase_order(task),
            task: task_info(task, &parents),
        })
        .collect();

    let facility_ref = |id: Uuid, name: &str| FacilityRef {
        facility_id: id,
        facility_name: name.to_string(),
    };
    Json(BulkTasksResponse {
        source: (&ctx.source).into(),
        parent: ctx.parent.as_ref().map(FacilityList::from),
        tasks,
        destinations: ctx
            .destinations
            .iter()
            .map(|d| facility_ref(d.facility_id, &d.facility_name))
            .collect(),
        unlinked: ctx
            .unlinked
            .iter()
            .map(|(id, name)| facility_ref(*id, name))
            .collect(),
    })
    .into_response()
}
