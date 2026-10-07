//! Marks a client company's implementation as completed (or reopens it).
//! The Clients page moves a completed company out of "Implementations in
//! Flight" into a collapsed "Completed Implementations" section; nothing
//! else about the company changes, and nothing here blocks any tool.
//!
//! A soft flag like archiving (`clients_companies::set_archived`): one
//! nullable timestamp, `clients.companies.implementation_completed_at`.
//! Gated to `client_ops.perform`, same as archive. Both directions are
//! idempotent -- marking an already-completed company keeps its original
//! timestamp and still answers 204 -- so a double click or a retry never
//! errors; only an unknown id is a 404.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use uuid::Uuid;

use crate::api::{internal_error, not_found, user_agent_from, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log;

const PERMISSION: &str = "client_ops.perform";

async fn set_completed(
    state: AppState,
    user: AuthenticatedUser,
    headers: HeaderMap,
    company_id: Uuid,
    completed: bool,
) -> Response {
    let user_agent = user_agent_from(&headers);

    if let Err(response) = user
        .require_permission(
            &state.db,
            PERMISSION,
            if completed {
                "mark_implementation_completed"
            } else {
                "reopen_implementation"
            },
            user_agent,
            None,
        )
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for the implementation-completed toggle");
            return internal_error("Could not update this client");
        }
    };

    let query = if completed {
        "UPDATE clients.companies \
            SET implementation_completed_at = COALESCE(implementation_completed_at, now()) \
          WHERE id = $1 RETURNING legal_name"
    } else {
        "UPDATE clients.companies SET implementation_completed_at = NULL \
          WHERE id = $1 RETURNING legal_name"
    };

    let updated: Result<Option<String>, sqlx::Error> = sqlx::query_scalar(query)
        .bind(company_id)
        .fetch_optional(&mut *tx)
        .await;

    let legal_name = match updated {
        Ok(Some(legal_name)) => legal_name,
        Ok(None) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back a no-op implementation toggle");
            }
            // An unknown id and a row RLS hides look the same, as
            // everywhere else in this codebase.
            return not_found("not_found", "Client not found.".to_string());
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, company_id = %company_id, "implementation-completed toggle failed");
            return internal_error("Could not update this client");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit the implementation-completed toggle");
        return internal_error("Could not update this client");
    }

    // After the commit, not before -- see fees.rs's update_fees for why.
    audit_log::record(
        &state.db,
        if completed {
            audit_log::event::CLIENT_IMPLEMENTATION_COMPLETED
        } else {
            audit_log::event::CLIENT_IMPLEMENTATION_REOPENED
        },
        user.user_id,
        "company",
        Some(&company_id.to_string()),
        audit_log::Change::none(),
        user_agent,
        None,
        serde_json::json!({ "legal_name": legal_name }),
    )
    .await;

    tracing::info!(user_id = %user.user_id, company_id = %company_id, completed, "user toggled a client's implementation-completed state");

    StatusCode::NO_CONTENT.into_response()
}

/// `PUT /clients/{company_id}/implementation-completed`
pub async fn mark_implementation_completed(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
) -> Response {
    set_completed(state, user, headers, company_id, true).await
}

/// `DELETE /clients/{company_id}/implementation-completed`
pub async fn reopen_implementation(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
) -> Response {
    set_completed(state, user, headers, company_id, false).await
}
