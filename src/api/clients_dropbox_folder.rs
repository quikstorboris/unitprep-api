//! Facility page's DropBox tab -- lets a manager change which Dropbox
//! folder a facility is linked to (the wrong facility folder got linked
//! at ingest time, or a client's own folder structure changed since).
//! Rare and deliberate, so every change is audit-logged the same way a
//! Merchant Account relink is (`api::clients_elavon`). The Company
//! page's own "Go to DropBox" links (one per facility, a launchpad
//! across all of a company's facilities) are unaffected by this tab --
//! Boris's own call, 2026-09-04: that list stays on the Company page,
//! not moved here just because this tab exists.
//!
//! Marking `dropbox_folder_url` into `manually_edited_fields` on write
//! means the existing scalar-field re-sync protection
//! (`clients::sync::apply_facility_refresh`) already covers this change
//! going forward -- no new protection mechanism needed, this field was
//! already one of the ones that mechanism guards.

use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log;

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(ApiErrorBody { error: "not_found", message: "No such facility.".to_string() }))
        .into_response()
}

fn request_context(headers: &HeaderMap) -> Option<&str> {
    headers.get(axum::http::header::USER_AGENT).and_then(|value| value.to_str().ok())
}

#[derive(Debug, Deserialize)]
pub struct UpdateDropboxFolderRequest {
    /// `None` clears the link entirely (back to "no folder linked").
    pub dropbox_folder_url: Option<String>,
}

/// No extra permission check beyond authentication -- same reasoning as
/// `clients_facility_people`'s own module doc: RLS already gates
/// `clients.facilities` UPDATE to `onboarding_manager`/`department_manager`.
pub async fn update_facility_dropbox_folder(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateDropboxFolderRequest>,
) -> Response {
    let user_agent = request_context(&headers);

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for update dropbox folder");
            return internal_error("Could not update this facility's Dropbox folder");
        }
    };

    let existing: Option<(Option<String>,)> =
        match sqlx::query_as("SELECT dropbox_folder_url FROM clients.facilities WHERE id = $1 AND company_id = $2")
            .bind(facility_id)
            .bind(company_id)
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(err) => {
                let _ = tx.rollback().await;
                tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for dropbox folder update failed");
                return internal_error("Could not update this facility's Dropbox folder");
            }
        };
    let Some((previous_url,)) = existing else {
        let _ = tx.rollback().await;
        return not_found();
    };

    if let Err(err) = sqlx::query(
        "UPDATE clients.facilities
            SET dropbox_folder_url = $1,
                manually_edited_fields = CASE
                    WHEN 'dropbox_folder_url' = ANY(manually_edited_fields) THEN manually_edited_fields
                    ELSE array_append(manually_edited_fields, 'dropbox_folder_url')
                END
          WHERE id = $2",
    )
    .bind(&request.dropbox_folder_url)
    .bind(facility_id)
    .execute(&mut *tx)
    .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "facility dropbox folder update failed");
        return internal_error("Could not update this facility's Dropbox folder");
    }

    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_DROPBOX_FOLDER_CHANGED,
        user.user_id,
        "facility",
        Some(&facility_id.to_string()),
        audit_log::Change::from_to(
            serde_json::json!(previous_url),
            serde_json::json!(&request.dropbox_folder_url),
        ),
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit dropbox folder update transaction");
        return internal_error("Could not update this facility's Dropbox folder");
    }

    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn update_facility_dropbox_folder_reaches_the_database() {
        let response = update_facility_dropbox_folder(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateDropboxFolderRequest {
                dropbox_folder_url: Some("https://www.dropbox.com/home/Some/Folder".to_string()),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
