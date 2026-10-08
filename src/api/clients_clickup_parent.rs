//! A company's ClickUp **parent facility** -- the one whose list is the
//! source when comments are copied to the company's other facilities'
//! lists (ClickUp Copy) -- and the "no ClickUp project" waiver.
//!
//! Both live on `clients.companies`; only this module writes them (the
//! Create screen's waiver checkbox goes through `clients::create`).
//! Gated to `client_ops.perform`, like the other company-level switches.
//! Every designation change is appended to
//! `clients.company_clickup_parent_history` in the same transaction as the
//! change, so the history can never disagree with the column.
//!
//! The parent must belong to the company **and** have a linked ClickUp
//! list: a parent with no list has nothing to copy from. (The link may be
//! removed later; the designation then stays, and ClickUp Copy reports
//! "parent has no list" rather than the designation silently vanishing.)
//! Setting the parent it already is, or clearing a parent that is not
//! set, is an idempotent 204.

use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::rls::{begin_for, try_response};
use crate::api::{bad_request, internal_error, not_found, user_agent_from, AppState};
use crate::auth::AuthenticatedUser;
use crate::client_ops::audit_log;

const PERMISSION: &str = "client_ops.perform";

#[derive(Debug, Deserialize)]
pub struct SetParentRequest {
    /// The facility to designate; `null` clears the designation.
    pub facility_id: Option<Uuid>,
}

/// `PUT /clients/{company_id}/clickup-parent`
pub async fn set_clickup_parent(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Json(request): Json<SetParentRequest>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    try_response!(
        user.require_permission(
            &state.db,
            PERMISSION,
            "set_clickup_parent",
            user_agent,
            None,
        )
        .await
    );

    let mut tx = try_response!(begin_for(&state, &user, "Could not set the parent facility").await);

    let current: Result<Option<(Option<Uuid>,)>, sqlx::Error> = sqlx::query_as(
        "SELECT clickup_parent_facility_id FROM clients.companies WHERE id = $1 FOR UPDATE",
    )
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await;

    let previous_id = match current {
        Ok(Some((previous_id,))) => previous_id,
        Ok(None) => {
            let _ = tx.rollback().await;
            return not_found("not_found", "Client not found.".to_string());
        }
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "company lookup failed while setting the ClickUp parent");
            return internal_error("Could not set the parent facility");
        }
    };

    if previous_id == request.facility_id {
        let _ = tx.rollback().await;
        return StatusCode::NO_CONTENT.into_response();
    }

    // The new parent's name (also proves it belongs to this company and
    // has a list) and the old one's, for the history snapshot.
    let new_parent = match request.facility_id {
        None => None,
        Some(facility_id) => {
            let row: Result<Option<(String, Option<String>)>, sqlx::Error> = sqlx::query_as(
                "SELECT name, clickup_list_id FROM clients.facilities \
                  WHERE id = $1 AND company_id = $2",
            )
            .bind(facility_id)
            .bind(company_id)
            .fetch_optional(&mut *tx)
            .await;

            match row {
                Ok(Some((name, Some(_)))) => Some((facility_id, name)),
                Ok(Some((name, None))) => {
                    let _ = tx.rollback().await;
                    return bad_request(
                        "parent_has_no_clickup_list",
                        format!("{name} has no ClickUp list linked yet. Link it first."),
                    );
                }
                Ok(None) => {
                    let _ = tx.rollback().await;
                    return bad_request(
                        "invalid_parent_facility",
                        "That facility does not belong to this client.".to_string(),
                    );
                }
                Err(err) => {
                    let _ = tx.rollback().await;
                    tracing::error!(error = %err, user_id = %user.user_id, "facility lookup failed while setting the ClickUp parent");
                    return internal_error("Could not set the parent facility");
                }
            }
        }
    };

    let previous_name: Option<String> = match previous_id {
        None => None,
        Some(previous_id) => {
            sqlx::query_scalar("SELECT name FROM clients.facilities WHERE id = $1")
                .bind(previous_id)
                .fetch_optional(&mut *tx)
                .await
                .unwrap_or(None)
        }
    };

    let write = async {
        sqlx::query("UPDATE clients.companies SET clickup_parent_facility_id = $1 WHERE id = $2")
            .bind(request.facility_id)
            .bind(company_id)
            .execute(&mut *tx)
            .await?;

        sqlx::query(
            "INSERT INTO clients.company_clickup_parent_history \
             (company_id, from_facility_id, from_facility_name, to_facility_id, to_facility_name, changed_by) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(company_id)
        .bind(previous_id)
        .bind(&previous_name)
        .bind(new_parent.as_ref().map(|(id, _)| *id))
        .bind(new_parent.as_ref().map(|(_, name)| name.as_str()))
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;

        Ok::<(), sqlx::Error>(())
    }
    .await;

    if let Err(err) = write {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "failed to write the ClickUp parent");
        return internal_error("Could not set the parent facility");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit the ClickUp parent");
        return internal_error("Could not set the parent facility");
    }

    // After the commit, not before -- see fees.rs's update_fees for why.
    audit_log::record(
        &state.db,
        audit_log::event::CLIENT_CLICKUP_PARENT_CHANGED,
        user.user_id,
        "company",
        Some(&company_id.to_string()),
        audit_log::Change::from_to(
            serde_json::json!({ "facility_id": previous_id, "facility_name": previous_name }),
            serde_json::json!({
                "facility_id": new_parent.as_ref().map(|(id, _)| id),
                "facility_name": new_parent.as_ref().map(|(_, name)| name),
            }),
        ),
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

    StatusCode::NO_CONTENT.into_response()
}

async fn set_waiver(
    state: AppState,
    user: AuthenticatedUser,
    headers: HeaderMap,
    company_id: Uuid,
    waived: bool,
) -> Response {
    let user_agent = user_agent_from(&headers);

    try_response!(
        user.require_permission(
            &state.db,
            PERMISSION,
            if waived {
                "waive_clickup_project"
            } else {
                "clear_clickup_waiver"
            },
            user_agent,
            None,
        )
        .await
    );

    let mut tx = try_response!(begin_for(&state, &user, "Could not update this client").await);

    // Idempotent both ways: waiving keeps the original who/when.
    let updated: Result<Option<String>, sqlx::Error> = if waived {
        sqlx::query_scalar(
            "UPDATE clients.companies \
                SET clickup_waived_by = CASE WHEN clickup_waived_at IS NULL THEN $2 ELSE clickup_waived_by END, \
                    clickup_waived_at = COALESCE(clickup_waived_at, now()) \
              WHERE id = $1 RETURNING legal_name",
        )
        .bind(company_id)
        .bind(user.user_id)
        .fetch_optional(&mut *tx)
        .await
    } else {
        sqlx::query_scalar(
            "UPDATE clients.companies SET clickup_waived_at = NULL, clickup_waived_by = NULL \
              WHERE id = $1 RETURNING legal_name",
        )
        .bind(company_id)
        .fetch_optional(&mut *tx)
        .await
    };

    let legal_name = match updated {
        Ok(Some(legal_name)) => legal_name,
        Ok(None) => {
            let _ = tx.rollback().await;
            return not_found("not_found", "Client not found.".to_string());
        }
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, company_id = %company_id, "ClickUp waiver update failed");
            return internal_error("Could not update this client");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit the ClickUp waiver");
        return internal_error("Could not update this client");
    }

    audit_log::record(
        &state.db,
        audit_log::event::CLIENT_CLICKUP_WAIVER_CHANGED,
        user.user_id,
        "company",
        Some(&company_id.to_string()),
        audit_log::Change::none(),
        user_agent,
        None,
        serde_json::json!({ "legal_name": legal_name, "waived": waived }),
    )
    .await;

    StatusCode::NO_CONTENT.into_response()
}

/// `PUT /clients/{company_id}/clickup-waiver` -- "no ClickUp project".
pub async fn waive_clickup_project(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
) -> Response {
    set_waiver(state, user, headers, company_id, true).await
}

/// `DELETE /clients/{company_id}/clickup-waiver`
pub async fn clear_clickup_waiver(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
) -> Response {
    set_waiver(state, user, headers, company_id, false).await
}
