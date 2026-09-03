//! "Add to OO" -- the real create trigger behind Phase 3's confirmation
//! screen. Wraps `clients::create::create_company_and_facilities`: given
//! one Intake run designated the company source (with its reviewed
//! `MappedCompany` fields) and zero or more designated facilities (each
//! with its own reviewed `EditableFacilityFields`), creates the real
//! `clients.companies`/`clients.facilities` rows. The reviewed values
//! come from `api::clients_preview`'s response, normally hand-edited by
//! whoever ran the import on the confirmation screen first.
//!
//! `company_intake_run_id` is free to also appear as one of
//! `facilities`' own `run_id`s -- see `clients::create`'s own doc
//! comment for why that's the normal case now, not an error.

use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log;
use crate::clients::create::{check_not_already_imported, fetch_create_data, write_create_data, CreateError, EditableFacilityFields};
use crate::clients::intake_mapping::MappedCompany;

const PERMISSION: &str = "client_ops.perform";

fn request_context(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
}

fn process_street_not_configured() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiErrorBody {
            error: "process_street_not_configured",
            message: "Process Street integration is not configured on this server.".to_string(),
        }),
    )
        .into_response()
}

fn already_imported(run_ids: Vec<String>) -> Response {
    (
        StatusCode::CONFLICT,
        Json(ApiErrorBody {
            error: "already_imported",
            message: format!(
                "Already in OO, not imported again: {}",
                run_ids.join(", ")
            ),
        }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
pub struct CreateFacilitySelection {
    pub run_id: String,
    pub fields: EditableFacilityFields,
    /// Carried straight through from `api::clients_preview`'s own
    /// `PreviewedRun::merchant_account_run_id` -- when present, Elavon
    /// data for this facility is ingested too. See `clients::create`'s
    /// own doc comment for why this exists.
    #[serde(default)]
    pub merchant_account_run_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateClientRequest {
    pub company_intake_run_id: String,
    pub company: MappedCompany,
    #[serde(default)]
    pub facilities: Vec<CreateFacilitySelection>,
}

#[derive(Debug, Serialize)]
pub struct CreateClientResponse {
    pub company_id: Uuid,
    pub facility_ids: Vec<Uuid>,
}

/// Requires `client_ops.perform` -- same standing permission the manual
/// PS sync trigger and the Process Street settings write both use; this
/// is the actual client-data-mutating action in the Process Street
/// integration, so it's the clearest fit for that gate.
pub async fn create_client(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Json(request): Json<CreateClientRequest>,
) -> Response {
    let user_agent = request_context(&headers);

    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "create_client_from_process_street", user_agent, None)
        .await
    {
        return response;
    }

    let company_intake_run_id = request.company_intake_run_id.trim();
    if company_intake_run_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "invalid_request",
                message: "company_intake_run_id is required.".to_string(),
            }),
        )
            .into_response();
    }

    let Some(client) = state.process_street.clone() else {
        return process_street_not_configured();
    };

    let facility_selections: Vec<(String, EditableFacilityFields, Option<String>)> = request
        .facilities
        .into_iter()
        .map(|selection| (selection.run_id, selection.fields, selection.merchant_account_run_id))
        .collect();

    let mut all_run_ids: Vec<&str> = vec![company_intake_run_id];
    all_run_ids.extend(facility_selections.iter().map(|(run_id, _, _)| run_id.as_str()));

    // --- Phase 1: fail fast on an already-imported run before ever
    // talking to Process Street, in its own short transaction. ---
    {
        let mut precheck_tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
            Ok(tx) => tx,
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for client creation pre-check");
                return internal_error("Could not create this client");
            }
        };
        match check_not_already_imported(&mut precheck_tx, &all_run_ids).await {
            Ok(()) => {
                if let Err(err) = precheck_tx.commit().await {
                    tracing::error!(error = %err, user_id = %user.user_id, "failed to commit client creation pre-check");
                    return internal_error("Could not create this client");
                }
            }
            Err(CreateError::AlreadyImported(run_ids)) => {
                let _ = precheck_tx.rollback().await;
                tracing::warn!(
                    user_id = %user.user_id,
                    run_ids = ?run_ids,
                    "client creation rejected: one or more selected runs are already in OO"
                );
                return already_imported(run_ids);
            }
            Err(err) => {
                let _ = precheck_tx.rollback().await;
                tracing::error!(error = %err, user_id = %user.user_id, "client creation pre-check failed");
                return internal_error("Could not create this client");
            }
        }
    }

    // --- Phase 2: the live Process Street round trip, deliberately with
    // no database transaction open (2026-09-03 fix -- this used to run
    // inside the same transaction Phase 3 below uses, holding a
    // database connection and its lock for however long PS took to
    // answer across every selected run; a slow response or a cancelled
    // request left the connection stuck `idle in transaction`, blocking
    // unrelated queries elsewhere in the app. Same root cause and fix as
    // `api::clients_elavon`'s own "link" action.) ---
    let fetched = match fetch_create_data(&client, company_intake_run_id, &facility_selections).await {
        Ok(fetched) => fetched,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to fetch runs from Process Street for client creation");
            return internal_error("Could not fetch these runs from Process Street");
        }
    };

    // --- Phase 3: the actual write, in a fresh transaction opened only
    // now that nothing left to do is network-bound. Re-checks
    // already-imported atomically with the write (see
    // `check_not_already_imported`'s own doc comment) -- Phase 1's own
    // check only protects against wasting a Process Street round trip
    // on an already-rejected request, not against a second request
    // racing in between. ---
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for client creation write");
            return internal_error("Could not create this client");
        }
    };

    let result =
        write_create_data(&mut tx, company_intake_run_id, &request.company, &facility_selections, &fetched).await;

    let created = match result {
        Ok(created) => created,
        Err(CreateError::AlreadyImported(run_ids)) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back an already-imported client creation attempt");
            }
            tracing::warn!(
                user_id = %user.user_id,
                run_ids = ?run_ids,
                "client creation rejected: one or more selected runs are already in OO"
            );
            return already_imported(run_ids);
        }
        Err(err) => {
            if let Err(rollback_err) = tx.rollback().await {
                tracing::error!(error = %rollback_err, "failed to roll back a failed client creation");
            }
            tracing::error!(error = %err, user_id = %user.user_id, "client creation failed");
            return internal_error("Could not create this client");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit client creation");
        return internal_error("Could not create this client");
    }

    audit_log::record(
        &state.db,
        audit_log::event::CLIENT_CREATED,
        user.user_id,
        "company",
        Some(&created.company_id.to_string()),
        audit_log::Change::none(),
        user_agent,
        None,
        serde_json::json!({
            "company_intake_run_id": company_intake_run_id,
            "facility_count": created.facility_ids.len(),
            "facility_ids": created.facility_ids,
        }),
    )
    .await;

    tracing::info!(
        user_id = %user.user_id,
        company_id = %created.company_id,
        facility_count = created.facility_ids.len(),
        "user created a client from Process Street"
    );

    (
        StatusCode::CREATED,
        Json(CreateClientResponse {
            company_id: created.company_id,
            facility_ids: created.facility_ids,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, onboarding_manager_user, test_user};

    #[tokio::test]
    async fn refuses_insufficient_permission_without_touching_anything() {
        let response = create_client(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Json(CreateClientRequest {
                company_intake_run_id: "abc123".to_string(),
                company: MappedCompany::default(),
                facilities: vec![],
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn rejects_a_blank_company_run_id_without_touching_anything() {
        let response = create_client(
            State(empty_state()),
            onboarding_manager_user(),
            HeaderMap::new(),
            Json(CreateClientRequest {
                company_intake_run_id: "   ".to_string(),
                company: MappedCompany::default(),
                facilities: vec![],
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn reports_not_configured_with_sufficient_permission_and_a_valid_request() {
        let response = create_client(
            State(empty_state()),
            onboarding_manager_user(),
            HeaderMap::new(),
            Json(CreateClientRequest {
                company_intake_run_id: "abc123".to_string(),
                company: MappedCompany::default(),
                facilities: vec![],
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
