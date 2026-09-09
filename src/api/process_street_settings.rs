//! Settings for the "Integrations" nav family -- Process Street today,
//! Dropbox/ClickUp/Claude etc. are follow-ups per the vault's own design
//! note. Two settings: how often the background sync runs
//! (`sync_interval_hours` -- `clients::sync::start_background_sync_task`
//! reads this same row on a system role to decide how long to sleep
//! before its next run, see that module's own doc comment) and the
//! integration's own API key (`api_key`, added 2026-09-09 alongside
//! `client_ops.dropbox_configuration`'s equivalent fields).
//!
//! **Was a fixed daily clock time (`sync_time`) until 2026-09-02** --
//! replaced with a plain hourly interval once it was clear the sync's
//! own delta mechanism makes a much tighter cadence realistic (an
//! unchanged run costs almost nothing beyond one shared list call), not
//! just a once-a-day compromise. See the migration's own comment
//! (`activity_logs_and_configurable_sync`) for the full reasoning.
//!
//! **Both read and write are admin-only (`integrations.manage`) as of
//! 2026-09-09** -- `get_settings` used to be any-authenticated (a plain
//! settings readout, no secret involved), but now that this row also
//! carries `api_key`, that's no longer true. This table's own RLS SELECT
//! policy stays any-authenticated regardless (the background sync's
//! system-role transaction still needs to read `sync_interval_hours`
//! through it -- see `20260909160000_add_process_street_api_key`'s own
//! comment); the app-layer check here is what actually keeps a non-admin
//! HTTP caller from ever seeing `api_key`.
//!
//! Like `dropbox_settings`, this returns the real, current `api_key` --
//! from the database if saved, else the `PROCESS_STREET_API_KEY` env var
//! (`state.env_source`) -- rather than a boolean, so the page shows what
//! Process Street access is actually running on. See that module's own
//! doc comment for the fuller reasoning; not repeated here.

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
use crate::integrations::secrets;

// Moved from client_ops.perform to integrations.manage (2026-09-09):
// Process Street/Dropbox settings became an admin-only "Integrations"
// nav section, per Boris's explicit call -- see the
// 20260909140000_add_integrations_manage_permission and
// 20260909150000_admin_only_integrations_settings migrations, the
// latter of which moves this table's RLS write policy to match.
const PERMISSION: &str = "integrations.manage";

/// See `dropbox_settings::AAD`'s identical reasoning.
const AAD: &[u8] = b"process_street_settings:1";

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

fn bad_request(error: &'static str, message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody { error, message }),
    )
        .into_response()
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    Database,
    Environment,
}

#[derive(Debug, Serialize)]
pub struct ProcessStreetSettingsResponse {
    pub sync_interval_hours: i16,
    pub api_key: String,
    pub api_key_source: ConfigSource,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct SettingsRow {
    sync_interval_hours: i16,
    api_key_ciphertext: Option<Vec<u8>>,
    updated_at: DateTime<Utc>,
    updated_by: Option<Uuid>,
}

fn resolve(
    row: SettingsRow,
    env_source: &dyn crate::integrations::env_source::EnvSource,
) -> Result<ProcessStreetSettingsResponse, String> {
    let (api_key, api_key_source) = match &row.api_key_ciphertext {
        Some(ciphertext) => (secrets::decrypt(AAD, ciphertext)?, ConfigSource::Database),
        None => (
            env_source.get("PROCESS_STREET_API_KEY").unwrap_or_default(),
            ConfigSource::Environment,
        ),
    };

    Ok(ProcessStreetSettingsResponse {
        sync_interval_hours: row.sync_interval_hours,
        api_key,
        api_key_source,
        updated_at: row.updated_at,
        updated_by: row.updated_by,
    })
}

pub async fn get_settings(State(state): State<AppState>, user: AuthenticatedUser) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "get_process_street_settings", None, None)
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for Process Street settings read");
            return internal_error("Could not load Process Street settings");
        }
    };

    let row: Result<SettingsRow, sqlx::Error> = sqlx::query_as(
        "SELECT sync_interval_hours, api_key_ciphertext, updated_at, updated_by FROM client_ops.process_street_settings WHERE id = 1",
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

    match resolve(row, state.env_source.as_ref()) {
        Ok(response) => Json(response).into_response(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to decrypt stored Process Street API key");
            internal_error("Could not load Process Street settings")
        }
    }
}

/// `api_key` is the real, current value the form was showing -- like
/// `dropbox_settings::UpdateDropboxSettingsRequest`, there's no "leave
/// unchanged" convention, saving just re-encrypts and stores exactly
/// what came in.
#[derive(Debug, Deserialize)]
pub struct UpdateProcessStreetSettingsRequest {
    pub sync_interval_hours: i16,
    pub api_key: String,
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
        return bad_request(
            "invalid_sync_interval_hours",
            format!("sync_interval_hours must be between {MIN_INTERVAL_HOURS} and {MAX_INTERVAL_HOURS}."),
        );
    }

    if request.api_key.is_empty() {
        return bad_request(
            "invalid_api_key",
            "API key is required.".to_string(),
        );
    }

    let api_key_ciphertext = match secrets::encrypt(AAD, &request.api_key) {
        Ok(blob) => blob,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to encrypt Process Street API key");
            return internal_error("Could not update Process Street settings");
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
            SET sync_interval_hours = $1, api_key_ciphertext = $2, updated_by = $3
          WHERE id = 1
      RETURNING sync_interval_hours, api_key_ciphertext, updated_at, updated_by",
    )
    .bind(request.sync_interval_hours)
    .bind(&api_key_ciphertext)
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
        "Process Street settings updated"
    );

    match resolve(row, state.env_source.as_ref()) {
        Ok(response) => Json(response).into_response(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to decrypt just-saved Process Street API key");
            internal_error("Could not update Process Street settings")
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::{admin_user, test_user};

    fn valid_request() -> UpdateProcessStreetSettingsRequest {
        UpdateProcessStreetSettingsRequest {
            sync_interval_hours: 24,
            api_key: "a-real-looking-key".to_string(),
        }
    }

    #[tokio::test]
    async fn update_refuses_insufficient_permission_without_touching_the_database() {
        let response = update_settings(
            State(crate::api::test_support::empty_state()),
            test_user(),
            HeaderMap::new(),
            Json(valid_request()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_refuses_insufficient_permission_without_touching_the_database() {
        let response = get_settings(State(crate::api::test_support::empty_state()), test_user()).await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn update_rejects_an_interval_below_the_minimum_without_touching_the_database() {
        let mut request = valid_request();
        request.sync_interval_hours = 0;

        let response = update_settings(
            State(crate::api::test_support::empty_state()),
            admin_user(),
            HeaderMap::new(),
            Json(request),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_rejects_an_interval_above_the_maximum_without_touching_the_database() {
        let mut request = valid_request();
        request.sync_interval_hours = 200;

        let response = update_settings(
            State(crate::api::test_support::empty_state()),
            admin_user(),
            HeaderMap::new(),
            Json(request),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_rejects_an_empty_api_key_without_touching_the_database() {
        let mut request = valid_request();
        request.api_key = String::new();

        let response = update_settings(
            State(crate::api::test_support::empty_state()),
            admin_user(),
            HeaderMap::new(),
            Json(request),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    fn row_without_a_saved_key() -> SettingsRow {
        SettingsRow {
            sync_interval_hours: 24,
            api_key_ciphertext: None,
            updated_at: Utc::now(),
            updated_by: None,
        }
    }

    #[test]
    fn resolve_falls_back_to_the_env_source_when_no_key_is_saved() {
        let env = crate::api::test_support::FakeEnvSource::with(&[(
            "PROCESS_STREET_API_KEY",
            "env-key",
        )]);

        let response = resolve(row_without_a_saved_key(), &env).expect("resolve must succeed");

        assert_eq!(response.api_key_source, ConfigSource::Environment);
        assert_eq!(response.api_key, "env-key");
    }

    #[test]
    #[serial_test::serial(integration_secrets_encryption_key_env)]
    fn resolve_prefers_a_saved_key_over_the_env_source() {
        std::env::set_var(
            "INTEGRATION_SECRETS_ENCRYPTION_KEY",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );

        let row = SettingsRow {
            sync_interval_hours: 24,
            api_key_ciphertext: Some(secrets::encrypt(AAD, "db-key").unwrap()),
            updated_at: Utc::now(),
            updated_by: None,
        };
        let env =
            crate::api::test_support::FakeEnvSource::with(&[("PROCESS_STREET_API_KEY", "env-key")]);

        let response = resolve(row, &env).expect("resolve must succeed");

        assert_eq!(response.api_key_source, ConfigSource::Database);
        assert_eq!(response.api_key, "db-key");

        std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
    }
}
