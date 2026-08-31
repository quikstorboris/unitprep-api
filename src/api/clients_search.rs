//! Search for a Process Street company/facility/person to import into
//! OO -- the entry point the "Add client from PS" flow (still Phase 3,
//! not built) will read from. Two genuinely different lookups running
//! side by side in one response:
//!
//! - **Facility matches**: a live call to PS's own server-side `name`
//!   filter over Intake runs only (`clients::search::search_by_facility_name`)
//!   -- cheap, no local index needed, always current. Each match also
//!   carries `already_imported`, a cheap local check against
//!   `clients.facilities.ps_intake_run_id` so a search result can be
//!   greyed out in the UI instead of silently inviting a duplicate
//!   import.
//! - **Person matches**: a local query against `clients.ps_person_index`
//!   (`clients::sync`'s delta-synced projection) -- PS has no
//!   server-side search over form-field values, so this is the only way
//!   to find a facility by an owner/DM/manager/signer/POC's name. Only
//!   as fresh as the last sync (`clients.ps_sync_state.last_synced_at`
//!   per run), not live.
//!
//! Requires only authentication, not a particular permission -- same
//! reasoning as `client_ops_qms_tags::list_qms_tags`: this is read-only
//! discovery data (facility/person names), not a client operation in
//! its own right.

use axum::{
    extract::{Json, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clients::search::search_by_facility_name;

#[derive(Debug, Deserialize)]
pub struct SearchClientsQuery {
    pub q: String,
}

#[derive(Debug, Serialize)]
pub struct FacilityMatch {
    pub run_id: String,
    pub run_name: String,
    pub status: String,
    /// Whether `clients.facilities` already has a row for this Intake
    /// run -- lets the UI grey this match out rather than inviting a
    /// duplicate "Add".
    pub already_imported: bool,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PersonMatch {
    /// `intake` | `merchant_account` | `contract_order`.
    pub workflow: String,
    pub ps_run_id: String,
    pub run_name: String,
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: String,
}

#[derive(Debug, Serialize)]
pub struct SearchClientsResponse {
    pub facility_matches: Vec<FacilityMatch>,
    pub person_matches: Vec<PersonMatch>,
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "invalid_search_query",
            message: message.to_string(),
        }),
    )
        .into_response()
}

fn process_street_not_configured() -> Response {
    tracing::warn!("client search attempted with Process Street not configured");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiErrorBody {
            error: "process_street_not_configured",
            message: "Process Street integration is not configured on this server.".to_string(),
        }),
    )
        .into_response()
}

pub async fn search_clients(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Query(query): Query<SearchClientsQuery>,
) -> Response {
    let q = query.q.trim();
    if q.is_empty() {
        return bad_request("q is required and must not be blank.");
    }

    let Some(client) = state.process_street.as_ref() else {
        return process_street_not_configured();
    };

    let facility_results = match search_by_facility_name(client, q).await {
        Ok(results) => results,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, query = %q, "Process Street facility-name search failed");
            return internal_error("Could not search Process Street");
        }
    };

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for search");
            return internal_error("Could not search Process Street");
        }
    };

    let candidate_run_ids: Vec<String> = facility_results.iter().map(|r| r.run_id.clone()).collect();
    let already_imported_result: Result<Vec<(String,)>, sqlx::Error> = sqlx::query_as(
        "SELECT ps_intake_run_id FROM clients.facilities WHERE ps_intake_run_id = ANY($1)",
    )
    .bind(&candidate_run_ids)
    .fetch_all(&mut *tx)
    .await;

    let already_imported: std::collections::HashSet<String> = match already_imported_result {
        Ok(rows) => rows.into_iter().map(|(id,)| id).collect(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "already-imported facility check failed");
            return internal_error("Could not search Process Street");
        }
    };

    let facility_matches: Vec<FacilityMatch> = facility_results
        .into_iter()
        .map(|r| FacilityMatch {
            already_imported: already_imported.contains(&r.run_id),
            run_id: r.run_id,
            run_name: r.run_name,
            status: r.status,
        })
        .collect();

    // A leading-wildcard ILIKE, not an indexed lookup -- see the
    // ps_person_index migration's own comment on why a full-text/trigram
    // index isn't worth it yet at this data's real scale. Capped at 50:
    // this is a picker, not a report, and an unbounded scan risk grows
    // with a substring query against every indexed run.
    let person_matches: Result<Vec<PersonMatch>, sqlx::Error> = sqlx::query_as(
        "SELECT workflow, ps_run_id, run_name, full_name, email, phone, role
           FROM clients.ps_person_index
          WHERE full_name ILIKE '%' || $1 || '%'
             OR email ILIKE '%' || $1 || '%'
          ORDER BY full_name
          LIMIT 50",
    )
    .bind(q)
    .fetch_all(&mut *tx)
    .await;

    let person_matches = match person_matches {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, query = %q, "person-index search query failed");
            return internal_error("Could not search for a person by name");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit person-name search transaction");
        return internal_error("Could not search for a person by name");
    }

    tracing::info!(
        user_id = %user.user_id,
        query = %q,
        facility_match_count = facility_matches.len(),
        person_match_count = person_matches.len(),
        "user searched for a Process Street client"
    );

    Json(SearchClientsResponse {
        facility_matches,
        person_matches,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn blank_query_is_rejected_without_touching_anything() {
        let response = search_clients(
            State(empty_state()),
            test_user(),
            Query(SearchClientsQuery { q: "   ".to_string() }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn missing_process_street_config_returns_service_unavailable() {
        // empty_state() carries process_street: None -- the same
        // "not configured" state a real deployment without
        // PROCESS_STREET_API_KEY set would have.
        let response = search_clients(
            State(empty_state()),
            test_user(),
            Query(SearchClientsQuery { q: "highway".to_string() }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
