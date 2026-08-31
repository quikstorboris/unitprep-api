//! Settings for the future "Integrations" nav family -- Process Street
//! today, Dropbox/ClickUp/Claude etc. are follow-ups per the vault's own
//! design note. Only one setting exists yet, per Boris's explicit scope
//! (2026-08-31): when the nightly person-index sync runs
//! (`client_ops.process_street_settings.sync_time`, a plain UTC
//! time-of-day). `clients::sync::start_background_sync_task` reads this
//! same row on a system role to decide how long to sleep before its
//! next run -- see that module's own doc comment.

use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};

const PERMISSION: &str = "client_ops.perform";

fn request_context(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
}

fn bad_request(message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "invalid_sync_time",
            message,
        }),
    )
        .into_response()
}

#[derive(Debug, Serialize)]
pub struct ProcessStreetSettingsResponse {
    /// `"HH:MM:SS"`, UTC -- see this module's own doc comment on why no
    /// timezone conversion happens here.
    pub sync_time: NaiveTime,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct SettingsRow {
    sync_time: NaiveTime,
    updated_at: DateTime<Utc>,
    updated_by: Option<Uuid>,
}

impl From<SettingsRow> for ProcessStreetSettingsResponse {
    fn from(row: SettingsRow) -> Self {
        Self {
            sync_time: row.sync_time,
            updated_at: row.updated_at,
            updated_by: row.updated_by,
        }
    }
}

/// Any authenticated caller -- a settings readout, not an action, same
/// reasoning as `clients_sync::sync_status`.
pub async fn get_settings(State(state): State<AppState>, user: AuthenticatedUser) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for Process Street settings read");
            return internal_error("Could not load Process Street settings");
        }
    };

    let row: Result<SettingsRow, sqlx::Error> = sqlx::query_as(
        "SELECT sync_time, updated_at, updated_by FROM client_ops.process_street_settings WHERE id = 1",
    )
    .fetch_one(&mut *tx)
    .await;

    let row = match row {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "Process Street settings read query failed");
            return internal_error("Could not load Process Street settings");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit Process Street settings read transaction");
        return internal_error("Could not load Process Street settings");
    }

    Json(ProcessStreetSettingsResponse::from(row)).into_response()
}

#[derive(Debug, Deserialize)]
pub struct UpdateProcessStreetSettingsRequest {
    /// `"HH:MM"` or `"HH:MM:SS"` -- validated below rather than relying
    /// on `NaiveTime`'s own `Deserialize` (which accepts formats a plain
    /// `<input type="time">` never sends and would surface as an
    /// unhelpful generic parse error).
    pub sync_time: String,
}

pub async fn update_settings(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Json(request): Json<UpdateProcessStreetSettingsRequest>,
) -> Response {
    let user_agent = request_context(&headers);

    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "update_process_street_settings", user_agent, None)
        .await
    {
        return response;
    }

    let sync_time = match NaiveTime::parse_from_str(&request.sync_time, "%H:%M:%S")
        .or_else(|_| NaiveTime::parse_from_str(&request.sync_time, "%H:%M"))
    {
        Ok(time) => time,
        Err(_) => {
            tracing::warn!(
                user_id = %user.user_id,
                sync_time = %request.sync_time,
                "Process Street settings update rejected: unparseable sync_time"
            );
            return bad_request(format!("{:?} is not a valid HH:MM time.", request.sync_time));
        }
    };

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for Process Street settings update");
            return internal_error("Could not update Process Street settings");
        }
    };

    let row: Result<SettingsRow, sqlx::Error> = sqlx::query_as(
        "UPDATE client_ops.process_street_settings
            SET sync_time = $1, updated_by = $2
          WHERE id = 1
      RETURNING sync_time, updated_at, updated_by",
    )
    .bind(sync_time)
    .bind(user.user_id)
    .fetch_one(&mut *tx)
    .await;

    let row = match row {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "Process Street settings update query failed");
            if let Err(rollback_err) = tx.rollback().await {
                tracing::error!(error = %rollback_err, "failed to roll back a failed Process Street settings update");
            }
            return internal_error("Could not update Process Street settings");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit Process Street settings update");
        return internal_error("Could not update Process Street settings");
    }

    tracing::info!(user_id = %user.user_id, sync_time = %sync_time, "Process Street sync time updated");

    Json(ProcessStreetSettingsResponse::from(row)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::test_user;

    #[tokio::test]
    async fn update_refuses_insufficient_permission_without_touching_the_database() {
        let response = update_settings(
            State(crate::api::test_support::empty_state()),
            test_user(),
            HeaderMap::new(),
            Json(UpdateProcessStreetSettingsRequest {
                sync_time: "00:00".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn update_rejects_an_unparseable_sync_time_without_touching_the_database() {
        let response = update_settings(
            State(crate::api::test_support::empty_state()),
            crate::api::test_support::onboarding_manager_user(),
            HeaderMap::new(),
            Json(UpdateProcessStreetSettingsRequest {
                sync_time: "not a time".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
