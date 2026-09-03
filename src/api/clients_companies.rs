//! Lists real `clients.companies` rows (with archiving) -- the data
//! behind the unified `/clients` page: the landing list after a
//! successful "Add to OO" create, and the entry point for Dedup/Unit
//! Groups/Template Tagger, which now key off a real company id instead
//! of the retired session-scoped client concept (see the vault's
//! Process Street Integration notes, 2026-09-01).
//!
//! Read (list) is any authenticated caller -- every tool under
//! `/clients/[clientId]/...` needs this list to even navigate, not just
//! onboarding staff. Archive/unarchive are real mutations, gated to
//! `client_ops.perform` same as create.

use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};

const PERMISSION: &str = "client_ops.perform";

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CompanySummary {
    pub id: Uuid,
    pub legal_name: String,
    pub created_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    /// Ordered, possibly empty -- enough for the list page to show
    /// "3 facilities: Highway 20, Carpentersville, Pyott Road" without
    /// a second round trip. Not paginated: this mirrors `facility_names`
    /// scale (a handful of sister facilities per real company, not
    /// hundreds), same assumption `clients.ps_person_index`'s own
    /// indexing already makes at this data's real size.
    pub facility_names: Vec<String>,
}

/// Any authenticated caller -- see this module's own doc comment.
pub async fn list_companies(State(state): State<AppState>, user: AuthenticatedUser) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for company list");
            return internal_error("Could not load clients");
        }
    };

    let companies: Result<Vec<CompanySummary>, sqlx::Error> = sqlx::query_as(
        "SELECT c.id, c.legal_name, c.created_at, c.archived_at,
                COALESCE(
                    array_agg(f.name ORDER BY f.name) FILTER (WHERE f.name IS NOT NULL),
                    '{}'
                ) AS facility_names
           FROM clients.companies c
           LEFT JOIN clients.facilities f ON f.company_id = c.id
          GROUP BY c.id
          ORDER BY c.archived_at IS NOT NULL, c.legal_name",
    )
    .fetch_all(&mut *tx)
    .await;

    let companies = match companies {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "company list query failed");
            return internal_error("Could not load clients");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit company list transaction");
        return internal_error("Could not load clients");
    }

    Json(companies).into_response()
}

async fn set_archived(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    company_id: Uuid,
    archive: bool,
) -> Response {
    if let Err(response) = user
        .require_permission(
            &state.db,
            PERMISSION,
            if archive { "archive_company" } else { "unarchive_company" },
            None,
            None,
        )
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for company archive toggle");
            return internal_error("Could not update this client");
        }
    };

    let query = if archive {
        "UPDATE clients.companies SET archived_at = now() WHERE id = $1 AND archived_at IS NULL RETURNING id"
    } else {
        "UPDATE clients.companies SET archived_at = NULL WHERE id = $1 AND archived_at IS NOT NULL RETURNING id"
    };

    let updated: Result<Option<(Uuid,)>, sqlx::Error> =
        sqlx::query_as(query).bind(company_id).fetch_optional(&mut *tx).await;

    let updated = match updated {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, company_id = %company_id, "company archive toggle failed");
            return internal_error("Could not update this client");
        }
    };

    if updated.is_none() {
        if let Err(err) = tx.rollback().await {
            tracing::error!(error = %err, "failed to roll back a no-op archive toggle");
        }
        // Either the id doesn't exist, or it's already in the requested
        // state -- either way there's nothing to report beyond 404, the
        // same "don't distinguish a real 404 from an RLS-filtered row"
        // reasoning this codebase already applies elsewhere.
        return (
            StatusCode::NOT_FOUND,
            Json(ApiErrorBody {
                error: "not_found",
                message: "Client not found, or already in the requested state.".to_string(),
            }),
        )
            .into_response();
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit company archive toggle");
        return internal_error("Could not update this client");
    }

    tracing::info!(user_id = %user.user_id, company_id = %company_id, archive, "user toggled a client's archived state");

    StatusCode::NO_CONTENT.into_response()
}

pub async fn archive_company(state: State<AppState>, user: AuthenticatedUser, Path(company_id): Path<Uuid>) -> Response {
    set_archived(state, user, company_id, true).await
}

pub async fn unarchive_company(
    state: State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
) -> Response {
    set_archived(state, user, company_id, false).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn archiving_refuses_insufficient_permission_without_touching_anything() {
        let response = archive_company(State(empty_state()), test_user(), Path(Uuid::new_v4())).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn unarchiving_refuses_insufficient_permission_without_touching_anything() {
        let response = unarchive_company(State(empty_state()), test_user(), Path(Uuid::new_v4())).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
