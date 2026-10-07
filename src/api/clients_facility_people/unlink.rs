//! `DELETE .../people/{id}` -- unlinks a person from a facility.

use crate::api::{internal_error, not_found, user_agent_from, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log;
use crate::clients::repository::unlink_person_from_facility;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct UnlinkFacilityPersonQuery {
    pub role: String,
}

/// Same no-extra-permission reasoning as `add_facility_person` above --
/// removing one link row is the same "this facility's own roster"
/// concern as adding one, not a `client_ops.perform`-gated action. No
/// live PS call, same restraint as `clients_elavon::unlink_facility_elavon`:
/// this only ever deletes `clients.facility_people`'s own link row (see
/// `repository::unlink_person_from_facility`'s own doc comment on why
/// `clients.people` itself is never touched).
pub async fn unlink_facility_person(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id, person_id)): Path<(Uuid, Uuid, Uuid)>,
    axum::extract::Query(query): axum::extract::Query<UnlinkFacilityPersonQuery>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for unlink facility person");
            return internal_error("Could not remove this person");
        }
    };

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
            tracing::error!(error = %err, user_id = %user.user_id, "facility existence check for unlink person failed");
            return internal_error("Could not remove this person");
        }
    };
    if facility_exists.is_none() {
        let _ = tx.rollback().await;
        return not_found("not_found", "No such facility.".to_string());
    }

    let removed: Option<(String, Option<String>, Option<String>)> = match sqlx::query_as(
        "SELECT p.full_name, p.email::text, p.phone
           FROM clients.facility_people fp
           JOIN clients.people p ON p.id = fp.person_id
          WHERE fp.facility_id = $1 AND fp.person_id = $2 AND fp.role = $3",
    )
    .bind(facility_id)
    .bind(person_id)
    .bind(&query.role)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, person_id = %person_id, "failed to read facility person state before unlink");
            return internal_error("Could not remove this person");
        }
    };

    if let Err(err) =
        unlink_person_from_facility(&mut tx, facility_id, person_id, &query.role).await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, facility_id = %facility_id, person_id = %person_id, "failed to unlink facility person");
        return internal_error("Could not remove this person");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit unlink facility person transaction");
        return internal_error("Could not remove this person");
    }

    // After the commit, not before -- see fees.rs's update_fees for why.
    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_PERSON_UNLINKED,
        user.user_id,
        "facility_person",
        Some(&person_id.to_string()),
        audit_log::Change::from_to(
            serde_json::json!(removed.map(|(full_name, email, phone)| {
                serde_json::json!({ "full_name": full_name, "email": email, "phone": phone, "role": query.role })
            })),
            serde_json::json!(null),
        ),
        user_agent,
        None,
        serde_json::json!({ "facility_id": facility_id }),
    )
    .await;

    StatusCode::NO_CONTENT.into_response()
}
