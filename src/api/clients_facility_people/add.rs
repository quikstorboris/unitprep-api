//! `POST .../people` -- links a person to a facility.

use super::SOURCES;
use crate::api::rls::{begin_for, try_response};
use crate::api::{bad_request, internal_error, not_found, user_agent_from, AppState};
use crate::auth::AuthenticatedUser;
use crate::client_ops::audit_log;
use crate::clients::people::PersonAssignment;
use crate::clients::repository::upsert_person_and_link_to_facility;
use axum::extract::{Json, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use uuid::Uuid;

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
#[derive(Debug, Deserialize)]
pub struct AddPersonRequest {
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: String,
    /// "process_street" for an "Add User" chip click, "manual" for a
    /// brand-new person typed in by hand and never touched by a future
    /// policy-style sync.
    pub source: String,
}

pub async fn add_facility_person(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<AddPersonRequest>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    if request.full_name.trim().is_empty() {
        return bad_request(
            "invalid_request",
            "full_name is required and must not be blank.".to_string(),
        );
    }
    if !SOURCES.contains(&request.source.as_str()) {
        return bad_request(
            "invalid_request",
            format!("\"{}\" is not a recognized source.", request.source),
        );
    }

    let assignment = PersonAssignment {
        full_name: request.full_name,
        email: request.email,
        phone: request.phone,
        role: request.role,
    };

    let mut tx = try_response!(begin_for(&state, &user, "Could not add this person").await);

    let facility_exists: Option<(Uuid,)> = match sqlx::query_as(
        "SELECT id FROM clients.facilities WHERE id = $1 AND company_id = $2",
    )
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
        return not_found("not_found", "No such facility.".to_string());
    }

    if let Err(err) =
        upsert_person_and_link_to_facility(&mut tx, facility_id, &assignment, &request.source).await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, facility_id = %facility_id, "failed to upsert facility person");
        return internal_error("Could not add this person");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit add facility person transaction");
        return internal_error("Could not add this person");
    }

    // After the commit, not before -- see fees.rs's update_fees for why.
    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_PERSON_ADDED,
        user.user_id,
        "facility_person",
        Some(&facility_id.to_string()),
        audit_log::Change::from_to(
            serde_json::json!(null),
            serde_json::json!({
                "full_name": assignment.full_name,
                "email": assignment.email,
                "phone": assignment.phone,
                "role": assignment.role,
                "source": request.source,
            }),
        ),
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

    StatusCode::NO_CONTENT.into_response()
}
