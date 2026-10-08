//! `GET /clients/{id}/facilities/{id}` -- one facility's own fields.

use crate::api::rls::{begin_for, try_response};
use crate::api::{internal_error, not_found, AppState};
use crate::auth::AuthenticatedUser;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::NaiveDate;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct FacilityDetailResponse {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub street_address: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub zip: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub units_count: Option<i32>,
    pub primary_storage_offering: Option<String>,
    pub previous_pms: Option<String>,
    pub access_control_system: Option<String>,
    pub go_live_date: Option<NaiveDate>,
    pub dropbox_folder_url: Option<String>,
    pub subdomain: Option<String>,
    pub subdomain_exists_in_qms_raw: Option<String>,
    pub system_email: Option<String>,
    pub website_url: Option<String>,
    pub clickup_list_id: Option<String>,
    pub clickup_list_name: Option<String>,
    pub clickup_folder_name: Option<String>,
    pub clickup_list_url: Option<String>,
}

/// Any authenticated caller -- General tab is plain facility contact
/// info, nothing sensitive.
pub async fn get_facility_detail(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let mut tx = try_response!(begin_for(&state, &user, "Could not load this facility").await);

    let facility: Option<FacilityDetailResponse> = match sqlx::query_as(
        "SELECT id, company_id, name, street_address, city, state, zip, phone, email, \
         units_count, primary_storage_offering, previous_pms, access_control_system, \
         go_live_date, dropbox_folder_url, subdomain, subdomain_exists_in_qms_raw, system_email, \
         website_url, clickup_list_id, clickup_list_name, clickup_folder_name, \
         clickup_list_url \
         FROM clients.facilities WHERE id = $1 AND company_id = $2",
    )
    .bind(facility_id)
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(facility) => facility,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility detail query failed");
            return internal_error("Could not load this facility");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit facility detail transaction");
        return internal_error("Could not load this facility");
    }

    match facility {
        Some(facility) => Json(facility).into_response(),
        None => not_found("not_found", "No such facility.".to_string()),
    }
}
