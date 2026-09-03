//! Settings for the future "Integrations" nav family -- Process Street
//! today, Dropbox/ClickUp/Claude etc. are follow-ups per the vault's own
//! design note. Only one setting exists yet: how often the background
//! sync runs (`client_ops.process_street_settings.sync_interval_hours`).
//! `clients::sync::start_background_sync_task` reads this same row on a
//! system role to decide how long to sleep before its next run -- see
//! that module's own doc comment.
//!
//! **Was a fixed daily clock time (`sync_time`) until 2026-09-02** --
//! replaced with a plain hourly interval once it was clear the sync's
//! own delta mechanism makes a much tighter cadence realistic (an
//! unchanged run costs almost nothing beyond one shared list call), not
//! just a once-a-day compromise. See the migration's own comment
//! (`activity_logs_and_configurable_sync`) for the full reasoning.

use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};

const PERMISSION: &str = "client_ops.perform";

/// Matches the `CHECK (sync_interval_hours BETWEEN 1 AND 168)` constraint
/// on `client_ops.process_street_settings` -- validated here too so a bad
/// value gets a clear 400 instead of surfacing as an opaque database
/// constraint-violation error.
const MIN_INTERVAL_HOURS: i16 = 1;
const MAX_INTERVAL_HOURS: i16 = 168;

fn request_context(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
}

fn bad_request(message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "invalid_sync_interval_hours",
            message,
        }),
    )
        .into_response()
}

#[derive(Debug, Serialize)]
pub struct ProcessStreetSettingsResponse {
    pub sync_interval_hours: i16,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct SettingsRow {
    sync_interval_hours: i16,
    updated_at: DateTime<Utc>,
    updated_by: Option<Uuid>,
}

impl From<SettingsRow> for ProcessStreetSettingsResponse {
    fn from(row: SettingsRow) -> Self {
        Self {
            sync_interval_hours: row.sync_interval_hours,
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
        "SELECT sync_interval_hours, updated_at, updated_by FROM client_ops.process_street_settings WHERE id = 1",
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
    pub sync_interval_hours: i16,
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

    if !(MIN_INTERVAL_HOURS..=MAX_INTERVAL_HOURS).contains(&request.sync_interval_hours) {
        tracing::warn!(
            user_id = %user.user_id,
            sync_interval_hours = request.sync_interval_hours,
            "Process Street settings update rejected: interval out of range"
        );
        return bad_request(format!(
            "sync_interval_hours must be between {MIN_INTERVAL_HOURS} and {MAX_INTERVAL_HOURS}."
        ));
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for Process Street settings update");
            return internal_error("Could not update Process Street settings");
        }
    };

    let row: Result<SettingsRow, sqlx::Error> = sqlx::query_as(
        "UPDATE client_ops.process_street_settings
            SET sync_interval_hours = $1, updated_by = $2
          WHERE id = 1
      RETURNING sync_interval_hours, updated_at, updated_by",
    )
    .bind(request.sync_interval_hours)
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

    tracing::info!(
        user_id = %user.user_id,
        sync_interval_hours = request.sync_interval_hours,
        "Process Street sync interval updated"
    );

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
            Json(UpdateProcessStreetSettingsRequest { sync_interval_hours: 24 }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn update_rejects_an_interval_below_the_minimum_without_touching_the_database() {
        let response = update_settings(
            State(crate::api::test_support::empty_state()),
            crate::api::test_support::onboarding_manager_user(),
            HeaderMap::new(),
            Json(UpdateProcessStreetSettingsRequest { sync_interval_hours: 0 }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_rejects_an_interval_above_the_maximum_without_touching_the_database() {
        let response = update_settings(
            State(crate::api::test_support::empty_state()),
            crate::api::test_support::onboarding_manager_user(),
            HeaderMap::new(),
            Json(UpdateProcessStreetSettingsRequest { sync_interval_hours: 200 }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
