//! Facility page's Users tab. `GET` returns two things: the facility's
//! actual saved roster (`clients.facility_people`/`clients.people`,
//! written once at ingest and never touched again automatically) and
//! "Add User" candidates -- rows already sitting in
//! `clients.ps_person_index` for this facility's own `ps_intake_run_id`,
//! kept fresh by the nightly background sync independent of when the
//! facility was created or last touched. No live Process Street call and
//! no search box: the candidates are exactly what PS currently says for
//! this facility's own Intake run, already indexed.
//!
//! `POST` is a chip click -- the request body is one candidate row (or
//! any equivalent hand-typed values) verbatim, and always upserts via
//! `repository::upsert_person_and_link_to_facility`: a person already
//! linked from an old ingest gets their stored name/phone overwritten
//! with the given values rather than left alone. Boris's own call
//! (2026-09-04): the saved roster should self-heal to whatever the fresh
//! index currently says on next use, not need a separate one-off
//! data-repair pass -- see that function's own doc comment for the real
//! example (Sand-Sto's "Irene Chen - (301) 787-9221") this fixes.

use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clients::people::PersonAssignment;
use crate::clients::repository::upsert_person_and_link_to_facility;

fn not_found(entity: &'static str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ApiErrorBody { error: "not_found", message: format!("No such {entity}.") }),
    )
        .into_response()
}

fn bad_request(message: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(ApiErrorBody { error: "invalid_request", message: message.to_string() }))
        .into_response()
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct FacilityPerson {
    pub person_id: Uuid,
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: String,
}

#[derive(Debug, Serialize)]
pub struct FacilityPeopleResponse {
    pub roster: Vec<FacilityPerson>,
    pub candidates: Vec<PersonAssignment>,
}

#[derive(sqlx::FromRow)]
struct FacilityIdentity {
    ps_intake_run_id: Option<String>,
}

/// Any authenticated caller -- same reasoning as `clients_elavon`'s own
/// GET: RLS's own SELECT policies (authenticated-only, no role check) are
/// the real backstop, matching every other read-only facility tab.
pub async fn get_facility_people(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for facility people");
            return internal_error("Could not load this facility's Users tab");
        }
    };

    let facility: Option<FacilityIdentity> =
        match sqlx::query_as("SELECT ps_intake_run_id FROM clients.facilities WHERE id = $1 AND company_id = $2")
            .bind(facility_id)
            .bind(company_id)
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for people tab failed");
                return internal_error("Could not load this facility's Users tab");
            }
        };
    let Some(facility) = facility else {
        let _ = tx.commit().await;
        return not_found("facility");
    };

    let roster: Vec<FacilityPerson> = match sqlx::query_as(
        "SELECT p.id AS person_id, p.full_name, p.email::text AS email, p.phone, fp.role
           FROM clients.facility_people fp
           JOIN clients.people p ON p.id = fp.person_id
          WHERE fp.facility_id = $1
          ORDER BY p.full_name",
    )
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility_people roster lookup failed");
            return internal_error("Could not load this facility's Users tab");
        }
    };

    // Same `workflow = 'intake'` scoping `clients_search`'s own
    // facility-person lookup uses -- a Merchant Account/Contract Order
    // person isn't part of this facility's own owner/DM/manager roster.
    let candidates: Vec<PersonAssignment> = match &facility.ps_intake_run_id {
        None => Vec::new(),
        Some(run_id) => match sqlx::query_as(
            "SELECT full_name, email, phone, role
               FROM clients.ps_person_index
              WHERE workflow = 'intake' AND ps_run_id = $1
              ORDER BY full_name",
        )
        .bind(run_id)
        .fetch_all(&mut *tx)
        .await
        {
            Ok(rows) => rows,
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "ps_person_index candidate lookup failed");
                return internal_error("Could not load this facility's Users tab");
            }
        },
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit facility people transaction");
        return internal_error("Could not load this facility's Users tab");
    }

    Json(FacilityPeopleResponse { roster, candidates }).into_response()
}

/// Requires no special permission beyond authentication -- same as every
/// other `clients.*` write, gated by RLS itself
/// (`onboarding_manager`/`department_manager` only, enforced at the
/// database level by the INSERT/UPDATE policies those tables already
/// carry), matching `clients_create`'s own reasoning rather than
/// `clients_elavon`'s `client_ops.perform` gate (that permission is
/// specific to actions this domain considers "performing a client
/// operation"; linking a person to a facility's own roster is closer to
/// the create-time confirmation screen's own People chips, which carry
/// no separate permission check of their own either).
pub async fn add_facility_person(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(assignment): Json<PersonAssignment>,
) -> Response {
    if assignment.full_name.trim().is_empty() {
        return bad_request("full_name is required and must not be blank.");
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for add facility person");
            return internal_error("Could not add this person");
        }
    };

    let facility_exists: Option<(Uuid,)> =
        match sqlx::query_as("SELECT id FROM clients.facilities WHERE id = $1 AND company_id = $2")
            .bind(facility_id)
            .bind(company_id)
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "facility existence check for add person failed");
                return internal_error("Could not add this person");
            }
        };
    if facility_exists.is_none() {
        let _ = tx.rollback().await;
        return not_found("facility");
    }

    if let Err(err) = upsert_person_and_link_to_facility(&mut tx, facility_id, &assignment).await {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, facility_id = %facility_id, "failed to upsert facility person");
        return internal_error("Could not add this person");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit add facility person transaction");
        return internal_error("Could not add this person");
    }

    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn get_facility_people_reaches_the_database() {
        let response =
            get_facility_people(State(empty_state()), test_user(), Path((Uuid::new_v4(), Uuid::new_v4()))).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn add_facility_person_rejects_a_blank_full_name_without_touching_the_database() {
        let response = add_facility_person(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(PersonAssignment {
                full_name: "   ".to_string(),
                email: Some("someone@example.com".to_string()),
                phone: None,
                role: "owner".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn add_facility_person_reaches_the_database() {
        let response = add_facility_person(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(PersonAssignment {
                full_name: "Irene Chen".to_string(),
                email: Some("irene@chenlawgroup.com".to_string()),
                phone: Some("(301) 787-9221".to_string()),
                role: "owner".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
