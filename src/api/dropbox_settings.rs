//! Settings for the Dropbox integration -- the app-wide credentials
//! `dropbox::DropboxConfig` loads at startup, previously exclusively
//! from `DROPBOX_*` env vars, now from `client_ops.dropbox_configuration`
//! first (see `dropbox::config::DropboxConfig::from_db`), falling back
//! to env vars when that row isn't fully configured. A saved change here
//! takes effect on the next server start, same as editing `.env.local`
//! did before this page existed -- unlike Process Street's sync
//! interval, nothing re-reads this mid-run (`DropboxClient` is
//! constructed once in `main.rs` and shared via `Arc`).
//!
//! Admin-only, both read and write (`integrations.manage`) -- unlike
//! `process_street_settings`' any-authenticated read, this table holds
//! `app_secret`/`refresh_token`, so even the read side stays admin-only.
//!
//! **The read response carries real, decrypted values** (masked/
//! revealed client-side, see `DropboxIntegrationPage` in `unitprep-ui`),
//! not booleans -- so the settings page can show what's actually
//! configured on first open instead of a blank form. When nothing has
//! been saved to this table yet, every field falls back to its
//! `DROPBOX_*` env var (`state.env_source`, see
//! `integrations::env_source`) -- the same values `DropboxConfig::
//! from_db`/`from_env` would resolve to at startup -- so the page always
//! reflects what Dropbox access is actually running on. `source` tells
//! the frontend which of the two it's looking at.
use axum::{
    extract::{Json, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{bad_request, internal_error, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::integrations::secrets;

const PERMISSION: &str = "integrations.manage";

/// See `dropbox::config::AAD`'s identical constant -- kept in sync by
/// hand since one lives beside `DropboxConfig` (used at startup) and
/// this one beside the settings handlers (used on save); both name the
/// same row.
const AAD: &[u8] = b"dropbox_configuration:1";

fn request_context(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    Database,
    Environment,
}

#[derive(Debug, Serialize)]
pub struct DropboxSettingsResponse {
    pub app_key: String,
    pub app_secret: String,
    pub refresh_token: String,
    pub root_namespace_id: String,
    pub root_path: String,
    pub source: ConfigSource,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct SettingsRow {
    app_key: Option<String>,
    app_secret_ciphertext: Option<Vec<u8>>,
    refresh_token_ciphertext: Option<Vec<u8>>,
    root_namespace_id: Option<String>,
    root_path: Option<String>,
    updated_at: DateTime<Utc>,
    updated_by: Option<Uuid>,
}

/// Resolves the row into what the page should actually show: the saved
/// values if a complete row exists (same all-or-nothing threshold
/// `DropboxConfig::from_db` uses), otherwise every `DROPBOX_*` env var
/// via `env_source` -- deliberately not a per-field mix of the two, so
/// the page never shows a combination that doesn't match any real,
/// resolvable `DropboxConfig`.
fn resolve(
    row: SettingsRow,
    env_source: &dyn crate::integrations::env_source::EnvSource,
) -> Result<DropboxSettingsResponse, String> {
    let saved = match (
        &row.app_key,
        &row.app_secret_ciphertext,
        &row.refresh_token_ciphertext,
        &row.root_namespace_id,
        &row.root_path,
    ) {
        (Some(app_key), Some(app_secret), Some(refresh_token), Some(root_namespace_id), Some(root_path)) => {
            Some((app_key, app_secret, refresh_token, root_namespace_id, root_path))
        }
        _ => None,
    };

    let (app_key, app_secret, refresh_token, root_namespace_id, root_path, source) = match saved {
        Some((app_key, app_secret_ciphertext, refresh_token_ciphertext, root_namespace_id, root_path)) => (
            app_key.clone(),
            secrets::decrypt(AAD, app_secret_ciphertext)?,
            secrets::decrypt(AAD, refresh_token_ciphertext)?,
            root_namespace_id.clone(),
            root_path.clone(),
            ConfigSource::Database,
        ),
        None => (
            env_source.get("DROPBOX_APP_KEY").unwrap_or_default(),
            env_source.get("DROPBOX_APP_SECRET").unwrap_or_default(),
            env_source.get("DROPBOX_REFRESH_TOKEN").unwrap_or_default(),
            env_source.get("DROPBOX_ROOT_NAMESPACE_ID").unwrap_or_default(),
            env_source.get("DROPBOX_ROOT_PATH").unwrap_or_default(),
            ConfigSource::Environment,
        ),
    };

    Ok(DropboxSettingsResponse {
        app_key,
        app_secret,
        refresh_token,
        root_namespace_id,
        root_path,
        source,
        updated_at: row.updated_at,
        updated_by: row.updated_by,
    })
}

pub async fn get_settings(State(state): State<AppState>, user: AuthenticatedUser) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "get_dropbox_settings", None, None)
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for Dropbox settings read");
            return internal_error("Could not load Dropbox settings");
        }
    };

    let row: Result<SettingsRow, sqlx::Error> = sqlx::query_as(
        "SELECT app_key, app_secret_ciphertext, refresh_token_ciphertext, root_namespace_id, root_path, updated_at, updated_by
           FROM client_ops.dropbox_configuration WHERE id = 1",
    )
    .fetch_one(&mut *tx)
    .await;

    let row = match row {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "Dropbox settings read query failed");
            return internal_error("Could not load Dropbox settings");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit Dropbox settings read transaction");
        return internal_error("Could not load Dropbox settings");
    }

    match resolve(row, state.env_source.as_ref()) {
        Ok(response) => Json(response).into_response(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to decrypt stored Dropbox settings");
            internal_error("Could not load Dropbox settings")
        }
    }
}

/// Every field is the real, current value the form was showing --
/// there's no "leave unchanged" convention here (see this module's own
/// doc comment on why the form is always pre-filled with the effective
/// value, never blank), so saving just re-encrypts and stores exactly
/// what came in.
#[derive(Debug, Deserialize)]
pub struct UpdateDropboxSettingsRequest {
    pub app_key: String,
    pub app_secret: String,
    pub refresh_token: String,
    pub root_namespace_id: String,
    pub root_path: String,
}

pub async fn update_settings(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Json(request): Json<UpdateDropboxSettingsRequest>,
) -> Response {
    let user_agent = request_context(&headers);

    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "update_dropbox_settings", user_agent, None)
        .await
    {
        return response;
    }

    if request.app_key.trim().is_empty()
        || request.app_secret.is_empty()
        || request.refresh_token.is_empty()
        || request.root_namespace_id.trim().is_empty()
        || request.root_path.trim().is_empty()
    {
        return bad_request(
            "invalid_dropbox_settings",
            "App key, app secret, refresh token, root namespace id, and root path are all required.".to_string(),
        );
    }

    let app_secret_ciphertext = match secrets::encrypt(AAD, &request.app_secret) {
        Ok(blob) => blob,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to encrypt Dropbox app secret");
            return internal_error("Could not update Dropbox settings");
        }
    };

    let refresh_token_ciphertext = match secrets::encrypt(AAD, &request.refresh_token) {
        Ok(blob) => blob,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to encrypt Dropbox refresh token");
            return internal_error("Could not update Dropbox settings");
        }
    };

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for Dropbox settings update");
            return internal_error("Could not update Dropbox settings");
        }
    };

    let row: Result<SettingsRow, sqlx::Error> = sqlx::query_as(
        "UPDATE client_ops.dropbox_configuration
            SET app_key = $1, app_secret_ciphertext = $2, refresh_token_ciphertext = $3,
                root_namespace_id = $4, root_path = $5, updated_by = $6
          WHERE id = 1
      RETURNING app_key, app_secret_ciphertext, refresh_token_ciphertext, root_namespace_id, root_path, updated_at, updated_by",
    )
    .bind(&request.app_key)
    .bind(&app_secret_ciphertext)
    .bind(&refresh_token_ciphertext)
    .bind(&request.root_namespace_id)
    .bind(&request.root_path)
    .bind(user.user_id)
    .fetch_one(&mut *tx)
    .await;

    let row = match row {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "Dropbox settings update query failed");
            if let Err(rollback_err) = tx.rollback().await {
                tracing::error!(error = %rollback_err, "failed to roll back a failed Dropbox settings update");
            }
            return internal_error("Could not update Dropbox settings");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit Dropbox settings update");
        return internal_error("Could not update Dropbox settings");
    }

    tracing::info!(user_id = %user.user_id, "Dropbox integration settings updated");

    match resolve(row, state.env_source.as_ref()) {
        Ok(response) => Json(response).into_response(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to decrypt just-saved Dropbox settings");
            internal_error("Could not update Dropbox settings")
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::test_user;

    fn valid_request() -> UpdateDropboxSettingsRequest {
        UpdateDropboxSettingsRequest {
            app_key: "key".to_string(),
            app_secret: "secret".to_string(),
            refresh_token: "token".to_string(),
            root_namespace_id: "ns".to_string(),
            root_path: "/path".to_string(),
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

    /// `developer` holds `integrations.manage` just like `admin` (see
    /// `20260909170000_add_developer_role`) -- this passes the same
    /// permission check `admin_user` would, then fails at the
    /// disconnected test pool exactly like every other `*_reaches_the_
    /// database` test in this codebase, proving it's not stuck at
    /// FORBIDDEN.
    #[tokio::test]
    async fn get_allows_a_developer_and_reaches_the_database() {
        let response = get_settings(
            State(crate::api::test_support::empty_state()),
            crate::api::test_support::developer_user(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    fn unconfigured_row() -> SettingsRow {
        SettingsRow {
            app_key: None,
            app_secret_ciphertext: None,
            refresh_token_ciphertext: None,
            root_namespace_id: None,
            root_path: None,
            updated_at: Utc::now(),
            updated_by: None,
        }
    }

    #[test]
    fn resolve_falls_back_to_the_env_source_when_nothing_is_saved() {
        let env = crate::api::test_support::FakeEnvSource::with(&[
            ("DROPBOX_APP_KEY", "env-key"),
            ("DROPBOX_APP_SECRET", "env-secret"),
            ("DROPBOX_REFRESH_TOKEN", "env-token"),
            ("DROPBOX_ROOT_NAMESPACE_ID", "env-ns"),
            ("DROPBOX_ROOT_PATH", "/env/path"),
        ]);

        let response = resolve(unconfigured_row(), &env).expect("resolve must succeed");

        assert_eq!(response.source, ConfigSource::Environment);
        assert_eq!(response.app_key, "env-key");
        assert_eq!(response.app_secret, "env-secret");
        assert_eq!(response.refresh_token, "env-token");
        assert_eq!(response.root_namespace_id, "env-ns");
        assert_eq!(response.root_path, "/env/path");
    }

    #[test]
    fn resolve_returns_empty_strings_when_neither_the_database_nor_the_env_source_has_anything() {
        let env = crate::api::test_support::FakeEnvSource::default();

        let response = resolve(unconfigured_row(), &env).expect("resolve must succeed");

        assert_eq!(response.source, ConfigSource::Environment);
        assert_eq!(response.app_key, "");
        assert_eq!(response.app_secret, "");
    }

    #[test]
    #[serial_test::serial(integration_secrets_encryption_key_env)]
    fn resolve_prefers_a_complete_saved_row_over_the_env_source() {
        std::env::set_var(
            "INTEGRATION_SECRETS_ENCRYPTION_KEY",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );

        let row = SettingsRow {
            app_key: Some("db-key".to_string()),
            app_secret_ciphertext: Some(secrets::encrypt(AAD, "db-secret").unwrap()),
            refresh_token_ciphertext: Some(secrets::encrypt(AAD, "db-token").unwrap()),
            root_namespace_id: Some("db-ns".to_string()),
            root_path: Some("/db/path".to_string()),
            updated_at: Utc::now(),
            updated_by: None,
        };
        let env = crate::api::test_support::FakeEnvSource::with(&[("DROPBOX_APP_KEY", "env-key")]);

        let response = resolve(row, &env).expect("resolve must succeed");

        assert_eq!(response.source, ConfigSource::Database);
        assert_eq!(response.app_key, "db-key");
        assert_eq!(response.app_secret, "db-secret");
        assert_eq!(response.refresh_token, "db-token");

        std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
    }
}
