//! Saving and removing facility -> ClickUp list links, the writing half
//! of the Company page's "Link ClickUp" flow (the read-only half --
//! suggestions, the dropdown's list catalog, resolving a pasted URL --
//! lives in `clickup_lookup`).
//!
//! Every list is **re-verified with ClickUp using the caller's own
//! token** before it is stored: the name and URL saved are what ClickUp
//! reports, never what the browser sent, and a list outside the
//! onboarding space is refused. Requires `integrations.clickup`; the
//! UPDATE itself is further bounded by the existing `clients.facilities`
//! RLS policy (client-ops roles), so a user who holds the ClickUp
//! permission but not a client-ops role gets a clear 403 instead of a
//! silent no-op.
//!
//! Several facilities may point at one list (nothing forbids it -- see
//! the migration's note), but the response reports it so the dialog can
//! warn: one-list-per-facility is the expected shape.

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::clickup_lookup::{token_and_hierarchy, verify_list, ListOption};
use crate::api::rls::{begin_for, try_response};
use crate::api::{bad_request, internal_error, not_found, user_agent_from, ApiErrorBody, AppState};
use crate::auth::AuthenticatedUser;
use crate::client_ops::audit_log;

const PERMISSION: &str = "integrations.clickup";

/// A company has at most a few dozen facilities; this only stops an
/// absurd request.
const MAX_LINKS: usize = 200;

/// How many ClickUp list lookups run at once when confirming a batch.
const VERIFY_CONCURRENCY: usize = 5;

#[derive(Debug, Deserialize)]
pub struct LinkRequest {
    pub facility_id: Uuid,
    pub list_id: String,
}

#[derive(Debug, Deserialize)]
pub struct SaveLinksRequest {
    pub links: Vec<LinkRequest>,
}

/// A list now shared by facilities beyond the ones just linked.
#[derive(Debug, Serialize)]
pub struct SharedList {
    pub list_id: String,
    pub list_name: String,
    pub also_linked_to: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SaveLinksResponse {
    pub linked: usize,
    pub shared_lists: Vec<SharedList>,
}

#[derive(Debug, Serialize)]
pub struct UnlinkResponse {
    pub unlinked: usize,
}

fn insufficient_role() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ApiErrorBody {
            error: "insufficient_role",
            message: "Your role cannot change a facility's ClickUp link.".to_string(),
        }),
    )
        .into_response()
}

fn link_state(list_id: Option<&str>, list_name: Option<&str>) -> serde_json::Value {
    serde_json::json!({ "clickup_list_id": list_id, "clickup_list_name": list_name })
}

pub async fn save_clickup_links(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Json(request): Json<SaveLinksRequest>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    if let Err(response) = user
        .require_permission(
            &state.db,
            PERMISSION,
            "save_clickup_links",
            user_agent,
            None,
        )
        .await
    {
        return response;
    }

    if request.links.is_empty() || request.links.len() > MAX_LINKS {
        return bad_request(
            "invalid_clickup_links",
            "Choose at least one facility and list to link.".to_string(),
        );
    }

    let mut seen = HashSet::new();
    if !request
        .links
        .iter()
        .all(|link| seen.insert(link.facility_id))
    {
        return bad_request(
            "invalid_clickup_links",
            "A facility can only be linked to one list at a time.".to_string(),
        );
    }
    if request
        .links
        .iter()
        .any(|link| link.list_id.trim().is_empty())
    {
        return bad_request(
            "invalid_clickup_links",
            "Every facility needs a list to link to.".to_string(),
        );
    }

    let (token, hierarchy) = match token_and_hierarchy(&state, &user).await {
        Ok(pair) => pair,
        Err(response) => return response,
    };

    // Verify each distinct list once, a few at a time. The first failure
    // (not found, wrong space, ClickUp down) ends the whole request --
    // nothing is saved unless every link is good.
    let list_ids: Vec<String> = {
        let mut unique: Vec<String> = Vec::new();
        for link in &request.links {
            let id = link.list_id.trim().to_string();
            if !unique.contains(&id) {
                unique.push(id);
            }
        }
        unique
    };

    let verified: Vec<Result<ListOption, Response>> = stream::iter(list_ids)
        .map(|list_id| {
            let (state, user, token, hierarchy) = (&state, &user, &token, &hierarchy);
            async move { verify_list(state, user, token, hierarchy, &list_id).await }
        })
        .buffer_unordered(VERIFY_CONCURRENCY)
        .collect()
        .await;

    let mut lists: HashMap<String, ListOption> = HashMap::new();
    for result in verified {
        match result {
            Ok(option) => {
                lists.insert(option.list_id.clone(), option);
            }
            Err(response) => return response,
        }
    }

    let mut tx = try_response!(begin_for(&state, &user, "Could not save the ClickUp links").await);

    let batch_facility_ids: Vec<Uuid> = request.links.iter().map(|l| l.facility_id).collect();
    let mut changes: Vec<(Uuid, Option<String>, ListOption)> = Vec::new();

    for link in &request.links {
        let Some(list) = lists.get(link.list_id.trim()) else {
            // verify_list returns the list ClickUp reports, whose id is
            // the canonical one; a pasted id always round-trips to it.
            return internal_error("Could not save the ClickUp links");
        };

        // Deliberately no FOR UPDATE: under RLS it would hide a facility
        // the caller may read but not write, turning "your role cannot
        // change this" (403, caught by the UPDATE below) into a
        // misleading "no such facility".
        let previous: Option<(Option<String>,)> = match sqlx::query_as(
            "SELECT clickup_list_id FROM clients.facilities WHERE id = $1 AND company_id = $2",
        )
        .bind(link.facility_id)
        .bind(company_id)
        .fetch_optional(&mut *tx)
        .await
        {
            Ok(row) => row,
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "facility lookup failed while saving ClickUp links");
                return internal_error("Could not save the ClickUp links");
            }
        };

        let Some((previous_list_id,)) = previous else {
            let _ = tx.rollback().await;
            return not_found(
                "not_found",
                "One of those facilities does not belong to this company.".to_string(),
            );
        };

        let updated = sqlx::query(
            "UPDATE clients.facilities
                SET clickup_list_id = $1, clickup_list_name = $2, clickup_folder_name = $3,
                    clickup_list_url = $4, clickup_linked_by = $5, clickup_linked_at = now()
              WHERE id = $6 AND company_id = $7",
        )
        .bind(&list.list_id)
        .bind(&list.list_name)
        .bind(&list.folder_name)
        .bind(&list.url)
        .bind(user.user_id)
        .bind(link.facility_id)
        .bind(company_id)
        .execute(&mut *tx)
        .await;

        match updated {
            // RLS silently narrows an UPDATE to zero rows for a role that
            // may not write facilities; surface that as a real 403.
            Ok(result) if result.rows_affected() == 0 => {
                let _ = tx.rollback().await;
                return insufficient_role();
            }
            Ok(_) => {}
            Err(err) => {
                let _ = tx.rollback().await;
                tracing::error!(error = %err, user_id = %user.user_id, "ClickUp link update failed");
                return internal_error("Could not save the ClickUp links");
            }
        }

        changes.push((link.facility_id, previous_list_id, list.clone()));
    }

    // Lists that other facilities (outside this batch) already use.
    let mut shared_lists = Vec::new();
    for list in lists.values() {
        let others: Result<Vec<(String,)>, sqlx::Error> = sqlx::query_as(
            "SELECT name FROM clients.facilities
              WHERE clickup_list_id = $1 AND id <> ALL($2) ORDER BY name",
        )
        .bind(&list.list_id)
        .bind(&batch_facility_ids)
        .fetch_all(&mut *tx)
        .await;

        match others {
            Ok(rows) if !rows.is_empty() => shared_lists.push(SharedList {
                list_id: list.list_id.clone(),
                list_name: list.list_name.clone(),
                also_linked_to: rows.into_iter().map(|(name,)| name).collect(),
            }),
            Ok(_) => {}
            Err(err) => {
                let _ = tx.rollback().await;
                tracing::error!(error = %err, user_id = %user.user_id, "shared-list lookup failed");
                return internal_error("Could not save the ClickUp links");
            }
        }
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit ClickUp links");
        return internal_error("Could not save the ClickUp links");
    }

    // After the commit, not before -- see fees.rs's update_fees for why.
    // One audit row per facility, written together (each is its own
    // database round trip; nine in a row is seconds of waiting).
    let (db, actor) = (&state.db, user.user_id);
    futures::future::join_all(changes.iter().map(
        |(facility_id, previous_list_id, list)| async move {
            audit_log::record(
                db,
                audit_log::event::FACILITY_CLICKUP_LINKED,
                actor,
                "facility",
                Some(&facility_id.to_string()),
                audit_log::Change::from_to(
                    link_state(previous_list_id.as_deref(), None),
                    link_state(Some(&list.list_id), Some(&list.list_name)),
                ),
                user_agent,
                None,
                serde_json::json!({ "company_id": company_id }),
            )
            .await;
        },
    ))
    .await;

    Json(SaveLinksResponse {
        linked: changes.len(),
        shared_lists,
    })
    .into_response()
}

pub async fn unlink_facility_clickup(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
) -> Response {
    unlink(state, user, headers, company_id, Some(facility_id)).await
}

pub async fn unlink_company_clickup(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
) -> Response {
    unlink(state, user, headers, company_id, None).await
}

/// Clears one facility's link (`facility_id`) or every link in the
/// company (`None`). Idempotent: nothing linked is not an error. Makes no
/// ClickUp call, so it works even when the user's token has gone bad --
/// removing a stale link should never require a working connection.
async fn unlink(
    state: AppState,
    user: AuthenticatedUser,
    headers: HeaderMap,
    company_id: Uuid,
    facility_id: Option<Uuid>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    let action = if facility_id.is_some() {
        "unlink_facility_clickup"
    } else {
        "unlink_company_clickup"
    };
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, action, user_agent, None)
        .await
    {
        return response;
    }

    let mut tx = try_response!(begin_for(&state, &user, "Could not remove the ClickUp link").await);

    // What is linked right now (also the audit "before").
    let linked: Result<Vec<(Uuid, String, String)>, sqlx::Error> = sqlx::query_as(
        "SELECT id, clickup_list_id, COALESCE(clickup_list_name, '')
           FROM clients.facilities
          WHERE company_id = $1 AND clickup_list_id IS NOT NULL
            AND ($2::uuid IS NULL OR id = $2)",
    )
    .bind(company_id)
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await;

    let linked = match linked {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "ClickUp link lookup failed");
            return internal_error("Could not remove the ClickUp link");
        }
    };

    if linked.is_empty() {
        let _ = tx.commit().await;
        return Json(UnlinkResponse { unlinked: 0 }).into_response();
    }

    let cleared = sqlx::query(
        "UPDATE clients.facilities
            SET clickup_list_id = NULL, clickup_list_name = NULL, clickup_folder_name = NULL,
                clickup_list_url = NULL, clickup_linked_by = NULL, clickup_linked_at = NULL
          WHERE company_id = $1 AND clickup_list_id IS NOT NULL
            AND ($2::uuid IS NULL OR id = $2)",
    )
    .bind(company_id)
    .bind(facility_id)
    .execute(&mut *tx)
    .await;

    match cleared {
        Ok(result) if result.rows_affected() as usize != linked.len() => {
            let _ = tx.rollback().await;
            return insufficient_role();
        }
        Ok(_) => {}
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "ClickUp unlink failed");
            return internal_error("Could not remove the ClickUp link");
        }
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit ClickUp unlink");
        return internal_error("Could not remove the ClickUp link");
    }

    let (db, actor) = (&state.db, user.user_id);
    futures::future::join_all(linked.iter().map(|(id, list_id, list_name)| async move {
        audit_log::record(
            db,
            audit_log::event::FACILITY_CLICKUP_UNLINKED,
            actor,
            "facility",
            Some(&id.to_string()),
            audit_log::Change::from_to(
                link_state(Some(list_id), Some(list_name)),
                link_state(None, None),
            ),
            user_agent,
            None,
            serde_json::json!({ "company_id": company_id }),
        )
        .await;
    }))
    .await;

    Json(UnlinkResponse {
        unlinked: linked.len(),
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::{clickup_user, empty_state, test_user};

    fn links(pairs: &[(Uuid, &str)]) -> Json<SaveLinksRequest> {
        Json(SaveLinksRequest {
            links: pairs
                .iter()
                .map(|(facility_id, list_id)| LinkRequest {
                    facility_id: *facility_id,
                    list_id: list_id.to_string(),
                })
                .collect(),
        })
    }

    #[tokio::test]
    async fn saving_refuses_a_caller_without_the_permission() {
        let response = save_clickup_links(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            links(&[(Uuid::new_v4(), "123")]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn unlinking_one_refuses_a_caller_without_the_permission() {
        let response = unlink_facility_clickup(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn unlinking_all_refuses_a_caller_without_the_permission() {
        let response = unlink_company_clickup(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn saving_rejects_an_empty_request() {
        let response = save_clickup_links(
            State(empty_state()),
            clickup_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            links(&[]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn saving_rejects_the_same_facility_twice() {
        let facility = Uuid::new_v4();
        let response = save_clickup_links(
            State(empty_state()),
            clickup_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            links(&[(facility, "1"), (facility, "2")]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn saving_rejects_a_blank_list_id() {
        let response = save_clickup_links(
            State(empty_state()),
            clickup_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            links(&[(Uuid::new_v4(), "   ")]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
