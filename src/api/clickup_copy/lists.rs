//! What every ClickUp Copy endpoint needs first: which facility's list is
//! the source, which is the target, and which is the company's parent
//! (named by the pointer comment); plus loading a list's tasks.

use std::collections::HashMap;
use std::sync::Arc;

use axum::response::Response;
use serde::Serialize;
use uuid::Uuid;

use crate::api::clickup_connection::{clickup_client, clickup_failure_response};
use crate::api::tool_runs::facility_belongs_to_company;
use crate::api::{bad_request, conflict, internal_error, not_found, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clickup::copy_pairing::Scope;
use crate::clickup::task_cache;
use crate::clickup::tasks::ClickUpTask;
use crate::clickup::ClickUpError;

pub(super) const PERMISSION: &str = "integrations.clickup";

/// A ClickUp comment is not worth more than this; stops an absurd body.
pub(super) const MAX_COMMENT_CHARS: usize = 10_000;

/// One facility's ClickUp list.
#[derive(Debug, Clone)]
pub(super) struct ListInfo {
    pub facility_id: Uuid,
    pub facility_name: String,
    pub list_id: String,
    pub list_name: String,
    pub list_url: String,
}

#[derive(Debug, Serialize)]
pub struct FacilityList {
    pub facility_id: Uuid,
    pub facility_name: String,
    pub list_name: String,
    pub list_url: String,
}

impl From<&ListInfo> for FacilityList {
    fn from(list: &ListInfo) -> Self {
        Self {
            facility_id: list.facility_id,
            facility_name: list.facility_name.clone(),
            list_name: list.list_name.clone(),
            list_url: list.list_url.clone(),
        }
    }
}

pub(super) struct Prepared {
    pub target: ListInfo,
    pub source: ListInfo,
    /// The company's designated parent, when it has a linked list: what
    /// the pointer comment names.
    pub parent: Option<ListInfo>,
}

pub(super) type FacilityRow = (Uuid, String, Option<String>, Option<String>, Option<String>);

pub(super) fn list_info(row: FacilityRow) -> Option<ListInfo> {
    let (facility_id, facility_name, list_id, list_name, list_url) = row;
    Some(ListInfo {
        facility_id,
        facility_name,
        list_id: list_id?,
        list_name: list_name?,
        list_url: list_url?,
    })
}

fn server_error(context: &'static str, err: &sqlx::Error, user: &AuthenticatedUser) -> Response {
    tracing::error!(error = %err, user_id = %user.user_id, "{context}");
    internal_error("Could not prepare ClickUp Copy")
}

/// Reads the target, source and parent facilities' lists, or the response
/// to return instead.
pub(super) async fn prepare(
    state: &AppState,
    user: &AuthenticatedUser,
    company_id: Uuid,
    facility_id: Uuid,
    source_facility_id: Option<Uuid>,
) -> Result<Prepared, Response> {
    let fail = |err: sqlx::Error| server_error("ClickUp Copy lookup failed", &err, user);

    let mut tx = begin_rls_transaction(&state.db, user.user_id, &user.role_keys)
        .await
        .map_err(fail)?;

    if !facility_belongs_to_company(&mut tx, facility_id, company_id)
        .await
        .map_err(fail)?
    {
        let _ = tx.commit().await;
        return Err(not_found("not_found", "No such facility.".to_string()));
    }

    let parent_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT clickup_parent_facility_id FROM clients.companies WHERE id = $1",
    )
    .bind(company_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(fail)?;

    let Some(source_id) = source_facility_id.or(parent_id) else {
        let _ = tx.commit().await;
        return Err(conflict(
            "no_source_facility",
            "Choose which facility to copy from, or designate a parent facility on the Company page."
                .to_string(),
        ));
    };
    if source_id == facility_id {
        let _ = tx.commit().await;
        return Err(bad_request(
            "source_is_target",
            "Choose a different facility to copy from: this is the one being copied to."
                .to_string(),
        ));
    }

    let wanted: Vec<Uuid> = [Some(facility_id), Some(source_id), parent_id]
        .into_iter()
        .flatten()
        .collect();
    let rows: Vec<FacilityRow> = sqlx::query_as(
        "SELECT id, name, clickup_list_id, clickup_list_name, clickup_list_url \
           FROM clients.facilities WHERE company_id = $1 AND id = ANY($2)",
    )
    .bind(company_id)
    .bind(&wanted)
    .fetch_all(&mut *tx)
    .await
    .map_err(fail)?;
    tx.commit().await.map_err(fail)?;

    let by_id: HashMap<Uuid, FacilityRow> = rows.into_iter().map(|row| (row.0, row)).collect();

    let Some(source_row) = by_id.get(&source_id).cloned() else {
        return Err(bad_request(
            "invalid_source_facility",
            "That facility does not belong to this client.".to_string(),
        ));
    };
    let source_name = source_row.1.clone();
    let Some(source) = list_info(source_row) else {
        return Err(conflict(
            "source_not_linked_to_clickup",
            format!("{source_name} has no ClickUp list linked, so there is nothing to copy from."),
        ));
    };

    let target_row = by_id
        .get(&facility_id)
        .cloned()
        .ok_or_else(|| not_found("not_found", "No such facility.".to_string()))?;
    let Some(target) = list_info(target_row) else {
        return Err(conflict(
            "facility_not_linked_to_clickup",
            "Link this facility to its ClickUp list first (the ClickUp section on its General tab)."
                .to_string(),
        ));
    };

    // The parent may be the source, the target or a third facility, and
    // has no pointer to name if it has no linked list.
    let parent = parent_id
        .and_then(|id| by_id.get(&id).cloned())
        .and_then(list_info);

    Ok(Prepared {
        target,
        source,
        parent,
    })
}

/// A list's tasks (cached for a few minutes, shared with the duplicate
/// check), or the response to return instead.
pub(super) async fn load_tasks(
    state: &AppState,
    user: &AuthenticatedUser,
    token: &str,
    list: &ListInfo,
) -> Result<Arc<Vec<ClickUpTask>>, Response> {
    match task_cache::get_or_load(user.user_id, &list.list_id, || async {
        clickup_client(state).list_tasks(token, &list.list_id).await
    })
    .await
    {
        Ok(tasks) => Ok(tasks),
        Err(ClickUpError::NotFound) => Err(not_found(
            "clickup_list_not_found",
            format!(
                "ClickUp no longer shows the linked list \"{}\". Re-link {}.",
                list.list_name, list.facility_name
            ),
        )),
        Err(err) => Err(clickup_failure_response(state, user, &err).await),
    }
}

/// Like [`load_tasks`], but a failure comes back as a message about that
/// one list, so a bulk view can show one unreachable facility without
/// failing the rest.
pub(super) async fn load_tasks_lenient(
    state: &AppState,
    user: &AuthenticatedUser,
    token: &str,
    list: &ListInfo,
) -> Result<Arc<Vec<ClickUpTask>>, String> {
    task_cache::get_or_load(user.user_id, &list.list_id, || async {
        clickup_client(state).list_tasks(token, &list.list_id).await
    })
    .await
    .map_err(|err| match err {
        ClickUpError::Unauthorized => "ClickUp rejected your token.".to_string(),
        ClickUpError::NotFound => format!(
            "ClickUp no longer shows the linked list \"{}\". Re-link {}.",
            list.list_name, list.facility_name
        ),
        other => format!("Could not read this facility's ClickUp list: {other}"),
    })
}

/// The Corp/Fac filter from the query string. The error is the ready-made
/// 400 response, like the other helpers here (and why the lint is allowed).
#[allow(clippy::result_large_err)]
pub(super) fn parse_scope(raw: Option<&str>) -> Result<Option<Scope>, Response> {
    match raw.map(str::trim) {
        None | Some("") | Some("all") => Ok(None),
        Some("corporate") => Ok(Some(Scope::Corporate)),
        Some("facility") => Ok(Some(Scope::Facility)),
        Some(_) => Err(bad_request(
            "invalid_scope",
            "Scope must be corporate, facility or all.".to_string(),
        )),
    }
}

pub(super) fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::Corporate => "corporate",
        Scope::Facility => "facility",
    }
}

pub(super) fn valid_task_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())
}
