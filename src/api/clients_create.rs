//! "Add to OO" -- the real create trigger behind Phase 3's confirmation
//! screen (still not built on the frontend). Wraps
//! `clients::create::create_company_and_facilities`: given one Intake
//! run designated the company source and zero or more designated
//! facilities, creates the real `clients.companies`/`clients.facilities`
//! rows.

use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clients::create::{create_company_and_facilities, CreateError};

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
pub struct CreateClientRequest {
    pub company_intake_run_id: String,
    #[serde(default)]
    pub facility_intake_run_ids: Vec<String>,
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

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for client creation");
            return internal_error("Could not create this client");
        }
    };

    let result = create_company_and_facilities(
        &client,
        &mut tx,
        company_intake_run_id,
        &request.facility_intake_run_ids,
    )
    .await;

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
                facility_intake_run_ids: vec![],
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
                facility_intake_run_ids: vec![],
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
                facility_intake_run_ids: vec![],
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
