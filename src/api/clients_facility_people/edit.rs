//! `PUT .../people/{id}` -- edits a facility person.

use crate::api::{bad_request, internal_error, not_found, user_agent_from, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log;
use crate::clients::repository::edit_person_and_facility_link;
use axum::extract::{Json, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct EditPersonRequest {
    pub old_role: String,
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: String,
    /// Only meaningful when this person's current link is
    /// 'process_street' -- see `repository::edit_person_and_facility_link`'s
    /// own doc comment. Ignored (already permanently protected) for an
    /// already-'manual' person.
    pub protect_from_resync: bool,
}

/// Same no-extra-permission reasoning as `add_facility_person` above.
/// The frontend already knows this person's current `source` (from the
/// roster `GET`), so it can decide whether to show the "protect from
/// resync" choice before ever submitting this request -- this handler
/// just carries out whatever was decided.
pub async fn edit_facility_person(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id, person_id)): Path<(Uuid, Uuid, Uuid)>,
    Json(request): Json<EditPersonRequest>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    if request.full_name.trim().is_empty() {
        return bad_request(
            "invalid_request",
            "full_name is required and must not be blank.".to_string(),
        );
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for edit facility person");
            return internal_error("Could not save this person");
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
            tracing::error!(error = %err, user_id = %user.user_id, "facility existence check for edit person failed");
            return internal_error("Could not save this person");
        }
    };
    if facility_exists.is_none() {
        let _ = tx.rollback().await;
        return not_found("not_found", "No such facility.".to_string());
    }

    let previous: Option<(String, Option<String>, Option<String>)> = match sqlx::query_as(
        "SELECT p.full_name, p.email::text, p.phone
           FROM clients.facility_people fp
           JOIN clients.people p ON p.id = fp.person_id
          WHERE fp.facility_id = $1 AND fp.person_id = $2 AND fp.role = $3",
    )
    .bind(facility_id)
    .bind(person_id)
    .bind(&request.old_role)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, person_id = %person_id, "failed to read prior facility person state");
            return internal_error("Could not save this person");
        }
    };

    let result = edit_person_and_facility_link(
        &mut tx,
        facility_id,
        person_id,
        &request.old_role,
        &request.full_name,
        request.email.as_deref(),
        request.phone.as_deref(),
        &request.role,
        request.protect_from_resync,
    )
    .await;

    match result {
        Ok(Some(_)) => {}
        Ok(None) => {
            let _ = tx.rollback().await;
            return not_found(
                "not_found",
                "No such person on this facility's roster.".to_string(),
            );
        }
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, person_id = %person_id, "failed to edit facility person");
            return internal_error("Could not save this person");
        }
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit edit facility person transaction");
        return internal_error("Could not save this person");
    }

    // After the commit, not before -- see fees.rs's update_fees for why.
    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_PERSON_UPDATED,
        user.user_id,
        "facility_person",
        Some(&person_id.to_string()),
        audit_log::Change::from_to(
            serde_json::json!(previous.map(|(full_name, email, phone)| {
                serde_json::json!({ "full_name": full_name, "email": email, "phone": phone, "role": request.old_role })
            })),
            serde_json::json!({
                "full_name": request.full_name,
                "email": request.email,
                "phone": request.phone,
                "role": request.role,
            }),
        ),
        user_agent,
        None,
        serde_json::json!({ "facility_id": facility_id }),
    )
    .await;

    StatusCode::NO_CONTENT.into_response()
}
