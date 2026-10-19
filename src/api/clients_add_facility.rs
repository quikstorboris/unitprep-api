//! Company page's "Add facility": attaches one more Process Street Intake
//! run to a company that already exists in OO. Built for the case where a
//! client's later facilities answered "have you submitted this form
//! before? yes", so nothing linked them to the company the first run
//! created, and for a group a manager treats as one company even though
//! its facilities sit under different legal entities in PS.
//!
//! Two endpoints:
//!
//! - `GET /clients/ps-runs?q=` -- a typeahead over Intake runs, answered
//!   **entirely from the local sync tables** (`ps_sync_state` run titles
//!   and `ps_person_index` names/emails), never a live Process Street
//!   call. PS allows ~0.7 requests/second (see `process_street::client`'s
//!   pacing), so a call per keystroke would starve the sync and every
//!   other PS user; the cost is that results are as fresh as the last
//!   sync. Each result says whether the run is already in OO and where.
//! - `POST /clients/{company_id}/facilities` -- imports the chosen run as
//!   a facility of this company, with PS's own values (edit them on the
//!   facility page afterwards). Same three phases as
//!   `clients_create::create_client`: a fail-fast duplicate check, the PS
//!   fetch with no transaction open, then one write transaction.

use axum::{
    extract::{Json, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::rls::{begin_for, try_response};
use crate::api::{
    bad_request, conflict, internal_error, not_found, process_street_not_configured,
    user_agent_from, AppState,
};
use crate::auth::AuthenticatedUser;
use crate::client_ops::audit_log;
use crate::clients::create::{
    check_not_already_imported, fetch_create_data, write_added_facility, CreateError,
    EditableFacilityFields,
};

const PERMISSION: &str = "client_ops.perform";
const MIN_QUERY_CHARS: usize = 2;
const RESULT_LIMIT: i64 = 20;

#[derive(Debug, Deserialize)]
pub struct PsRunSearchQuery {
    #[serde(default)]
    pub q: String,
}

/// Where a run that is already in OO lives.
#[derive(Debug, Serialize, sqlx::FromRow, ts_rs::TS)]
#[ts(export)]
pub struct PsRunAlreadyIn {
    pub company_id: Uuid,
    pub company_name: String,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct PsRunMatch {
    pub run_id: String,
    pub run_name: String,
    /// The person (owner, manager, ...) whose name or email matched, when
    /// the run's own title did not.
    pub matched_person: Option<String>,
    pub already_in: Option<PsRunAlreadyIn>,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct PsRunSearchResponse {
    pub matches: Vec<PsRunMatch>,
}

#[derive(sqlx::FromRow)]
struct RunRow {
    ps_run_id: String,
    run_name: String,
    person: Option<String>,
    company_id: Option<Uuid>,
    company_name: Option<String>,
}

/// Escapes `\`, `%` and `_` so the query is matched literally by ILIKE.
fn like_pattern(q: &str) -> String {
    let escaped = q
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

/// `GET /clients/ps-runs` -- any authenticated caller, like `/clients/search`.
pub async fn search_ps_runs(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Query(query): Query<PsRunSearchQuery>,
) -> Response {
    let q = query.q.trim();
    if q.chars().count() < MIN_QUERY_CHARS {
        return Json(PsRunSearchResponse { matches: vec![] }).into_response();
    }

    let mut tx =
        try_response!(begin_for(&state, &user, "Could not search Process Street runs").await);

    // A run's title match outranks a person match (rank 0 vs 1), and each
    // run appears once.
    let rows: Result<Vec<RunRow>, sqlx::Error> = sqlx::query_as(
        "WITH hits AS (
             SELECT ps_run_id, run_name, NULL::text AS person, 0 AS rank
               FROM clients.ps_sync_state
              WHERE workflow = 'intake' AND run_name ILIKE $1
             UNION ALL
             SELECT ps_run_id, run_name, full_name, 1
               FROM clients.ps_person_index
              WHERE workflow = 'intake' AND (full_name ILIKE $1 OR email ILIKE $1)
         ), best AS (
             SELECT DISTINCT ON (ps_run_id) ps_run_id, run_name, person
               FROM hits ORDER BY ps_run_id, rank
         )
         SELECT b.ps_run_id, b.run_name, b.person, c.id AS company_id, c.legal_name AS company_name
           FROM best b
           LEFT JOIN clients.facilities f ON f.ps_intake_run_id = b.ps_run_id
           LEFT JOIN clients.companies c ON c.id = f.company_id
          ORDER BY b.run_name
          LIMIT $2",
    )
    .bind(like_pattern(q))
    .bind(RESULT_LIMIT)
    .fetch_all(&mut *tx)
    .await;
    let _ = tx.rollback().await;

    match rows {
        Ok(rows) => Json(PsRunSearchResponse {
            matches: rows
                .into_iter()
                .map(|row| PsRunMatch {
                    run_id: row.ps_run_id,
                    run_name: row.run_name,
                    matched_person: row.person,
                    already_in: row.company_id.zip(row.company_name).map(
                        |(company_id, company_name)| PsRunAlreadyIn {
                            company_id,
                            company_name,
                        },
                    ),
                })
                .collect(),
        })
        .into_response(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "Process Street run typeahead failed");
            internal_error("Could not search Process Street runs")
        }
    }
}

#[derive(Debug, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct AddFacilityRequest {
    pub run_id: String,
    /// The facility's New Merchant Account run, when it has one.
    #[serde(default)]
    pub merchant_account_run_id: Option<String>,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct AddFacilityResponse {
    pub facility_id: Uuid,
}

fn already_imported(run_ids: Vec<String>) -> Response {
    conflict(
        "already_imported",
        format!("Already in OO, not imported again: {}", run_ids.join(", ")),
    )
}

async fn company_exists(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    company_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let found: Option<Uuid> = sqlx::query_scalar("SELECT id FROM clients.companies WHERE id = $1")
        .bind(company_id)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(found.is_some())
}

/// `POST /clients/{company_id}/facilities`
pub async fn add_facility(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Json(request): Json<AddFacilityRequest>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    try_response!(
        user.require_permission(&state.db, PERMISSION, "add_facility", user_agent, None)
            .await
    );

    let run_id = request.run_id.trim().to_string();
    if run_id.is_empty() {
        return bad_request("invalid_request", "run_id is required.".to_string());
    }
    let merchant_account_run_id = request
        .merchant_account_run_id
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty());

    let Some(client) = state.process_street.clone() else {
        return process_street_not_configured();
    };

    // Phase 1: the company exists and the run is not already in OO,
    // before spending a Process Street round trip.
    {
        let mut tx = try_response!(begin_for(&state, &user, "Could not add this facility").await);
        match company_exists(&mut tx, company_id).await {
            Ok(true) => {}
            Ok(false) => {
                let _ = tx.rollback().await;
                return not_found("not_found", "Client not found.".to_string());
            }
            Err(err) => {
                let _ = tx.rollback().await;
                tracing::error!(error = %err, user_id = %user.user_id, "company lookup for add-facility failed");
                return internal_error("Could not add this facility");
            }
        }
        let checked = check_not_already_imported(&mut tx, &[run_id.as_str()]).await;
        let _ = tx.rollback().await;
        match checked {
            Ok(()) => {}
            Err(CreateError::AlreadyImported(ids)) => return already_imported(ids),
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "add-facility pre-check failed");
                return internal_error("Could not add this facility");
            }
        }
    }

    // Phase 2: Process Street, no transaction open.
    let selection = (
        run_id.clone(),
        EditableFacilityFields::default(),
        merchant_account_run_id.clone(),
    );
    let fetched = match fetch_create_data(&client, &run_id, std::slice::from_ref(&selection)).await
    {
        Ok(fetched) => fetched,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to fetch runs from Process Street for add-facility");
            return internal_error("Could not fetch this run from Process Street");
        }
    };

    // Phase 3: the write, re-checking for a duplicate atomically.
    let mut tx = try_response!(begin_for(&state, &user, "Could not add this facility").await);
    let written = write_added_facility(
        &mut tx,
        company_id,
        &run_id,
        merchant_account_run_id.as_deref(),
        &fetched,
    )
    .await;
    let facility_id = match written {
        Ok(id) => id,
        Err(CreateError::AlreadyImported(ids)) => {
            let _ = tx.rollback().await;
            return already_imported(ids);
        }
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, company_id = %company_id, "add-facility failed");
            return internal_error("Could not add this facility");
        }
    };
    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit add-facility");
        return internal_error("Could not add this facility");
    }

    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_ADDED_TO_CLIENT,
        user.user_id,
        "company",
        Some(&company_id.to_string()),
        audit_log::Change::none(),
        user_agent,
        None,
        serde_json::json!({
            "facility_id": facility_id,
            "intake_run_id": run_id,
            "merchant_account_run_id": merchant_account_run_id,
        }),
    )
    .await;

    (
        StatusCode::CREATED,
        Json(AddFacilityResponse { facility_id }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, onboarding_manager_user, test_user};

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(like_pattern("50%_off\\"), "%50\\%\\_off\\\\%");
        assert_eq!(like_pattern("moss"), "%moss%");
    }

    #[tokio::test]
    async fn a_short_query_returns_no_matches_without_touching_the_database() {
        let response = search_ps_runs(
            State(empty_state()),
            test_user(),
            Query(PsRunSearchQuery {
                q: " a ".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn add_facility_refuses_insufficient_permission() {
        let response = add_facility(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            Json(AddFacilityRequest {
                run_id: "abc".to_string(),
                merchant_account_run_id: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn add_facility_rejects_a_blank_run_id() {
        let response = add_facility(
            State(empty_state()),
            onboarding_manager_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            Json(AddFacilityRequest {
                run_id: "  ".to_string(),
                merchant_account_run_id: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_facility_reports_process_street_not_configured() {
        let response = add_facility(
            State(empty_state()),
            onboarding_manager_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            Json(AddFacilityRequest {
                run_id: "abc".to_string(),
                merchant_account_run_id: None,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
