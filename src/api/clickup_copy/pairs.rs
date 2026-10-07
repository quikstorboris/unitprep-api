//! `GET .../clickup/copy-pairs`: the source and target lists' tasks in the
//! Set Up and Migration phases, paired by name/phase/parent. Suggestions
//! only -- the dialog lets the person override every row.

use std::collections::HashMap;

use axum::{
    extract::{Json, Path, Query, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::lists::{load_tasks, parse_scope, prepare, scope_name, FacilityList, PERMISSION};
use crate::api::clickup_connection::load_user_token;
use crate::api::AppState;
use crate::auth::AuthenticatedUser;
use crate::clickup::copy_pairing;
use crate::clickup::task_matching;
use crate::clickup::tasks::ClickUpTask;

#[derive(Debug, Deserialize)]
pub struct PairsQuery {
    pub source_facility_id: Option<Uuid>,
    /// `corporate`, `facility` or `all` (the default).
    pub scope: Option<String>,
}

/// A task as the dialog shows it.
#[derive(Debug, Serialize)]
pub struct TaskInfo {
    pub task_id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub parent_name: Option<String>,
    pub status: String,
    pub is_finished: bool,
    pub url: String,
    /// `corporate` / `facility`, when the task's Corp/Fac says.
    pub scope: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct TargetChoice {
    #[serde(flatten)]
    pub task: TaskInfo,
    pub score: f64,
}

#[derive(Debug, Serialize)]
pub struct PairRow {
    /// The phase ("Set Up", "Migration") the dialog groups rows under.
    pub phase: String,
    pub source: TaskInfo,
    /// The suggested counterpart; `None` is "no match".
    pub target: Option<TargetChoice>,
    pub alternatives: Vec<TargetChoice>,
}

#[derive(Debug, Serialize)]
pub struct PairsResponse {
    pub source: FacilityList,
    pub target: FacilityList,
    /// The company's main list, named by the pointer comment; `None` when
    /// no parent is designated (no pointer is posted then).
    pub parent: Option<FacilityList>,
    pub rows: Vec<PairRow>,
    /// Every eligible task in the target list, for choosing a different
    /// counterpart by hand.
    pub target_tasks: Vec<TaskInfo>,
}

pub(super) fn task_info(task: &ClickUpTask, parents: &HashMap<&str, &str>) -> TaskInfo {
    TaskInfo {
        task_id: task.id.clone(),
        name: task.name.trim().to_string(),
        parent_id: task.parent_id.clone(),
        parent_name: task
            .parent_id
            .as_deref()
            .and_then(|id| parents.get(id))
            .map(|name| name.trim().to_string()),
        status: task.status.clone(),
        is_finished: task.is_finished(),
        url: task.url.clone(),
        scope: copy_pairing::scope(task).map(scope_name),
    }
}

pub(super) fn target_choice(
    candidate: &copy_pairing::Candidate<'_>,
    parents: &HashMap<&str, &str>,
) -> TargetChoice {
    TargetChoice {
        task: task_info(candidate.task, parents),
        score: (candidate.score * 100.0).round() / 100.0,
    }
}

/// `GET .../clickup/copy-pairs`
pub async fn copy_pairs(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Query(query): Query<PairsQuery>,
) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "clickup_copy_pairs", None, None)
        .await
    {
        return response;
    }
    let scope = match parse_scope(query.scope.as_deref()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    let (prepared, token) = match tokio::try_join!(
        prepare(
            &state,
            &user,
            company_id,
            facility_id,
            query.source_facility_id
        ),
        load_user_token(&state, &user)
    ) {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    let (source_tasks, target_tasks) = match tokio::try_join!(
        load_tasks(&state, &user, &token, &prepared.source),
        load_tasks(&state, &user, &token, &prepared.target)
    ) {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    let source_parents = task_matching::parent_names(&source_tasks);
    let target_parents = task_matching::parent_names(&target_tasks);

    let rows = copy_pairing::pair(&source_tasks, &target_tasks, scope)
        .into_iter()
        .map(|pairing| PairRow {
            phase: copy_pairing::copy_phase(pairing.source)
                .unwrap_or_default()
                .to_string(),
            source: task_info(pairing.source, &source_parents),
            target: pairing
                .target
                .as_ref()
                .map(|c| target_choice(c, &target_parents)),
            alternatives: pairing
                .alternatives
                .iter()
                .map(|c| target_choice(c, &target_parents))
                .collect(),
        })
        .collect();

    Json(PairsResponse {
        source: (&prepared.source).into(),
        target: (&prepared.target).into(),
        parent: prepared.parent.as_ref().map(FacilityList::from),
        rows,
        target_tasks: copy_pairing::eligible(&target_tasks, scope)
            .into_iter()
            .map(|task| task_info(task, &target_parents))
            .collect(),
    })
    .into_response()
}
