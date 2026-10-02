//! A user's own ClickUp connection -- save/replace the personal API
//! token, re-test it, remove it, and read its status. Every endpoint
//! requires `integrations.clickup` (a permission an administrator grants
//! per user, see `auth_user_permissions`) and only ever touches the
//! *calling user's own* row in `integrations.user_clickup_credentials`;
//! RLS enforces that independently of this file.
//!
//! The token is validated against ClickUp before it is stored (a token
//! ClickUp rejects is never saved), encrypted at rest with
//! `integrations::secrets` under an AAD bound to the owning user, and
//! **never returned to the browser** -- the responses carry status and
//! the ClickUp identity it resolved to, nothing more.
//!
//! `status` semantics: `connected` means ClickUp accepted the token the
//! last time we checked; `invalid` means ClickUp rejected it (revoked,
//! regenerated, account deactivated). A *network* failure talking to
//! ClickUp is reported as a 502 and deliberately does NOT change the
//! stored status -- it says nothing about the token.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{bad_request, internal_error, ApiErrorBody, AppState};
use crate::auth::{audit_log, begin_rls_transaction, AuthenticatedUser};
use crate::clickup::{ClickUpClient, ClickUpError, ClickUpIdentity};
use crate::integrations::secrets;

const PERMISSION: &str = "integrations.clickup";

/// A ClickUp personal token is ~40 characters; this only exists to
/// refuse an absurd paste (a whole document) before it reaches the
/// network or the database.
const MAX_TOKEN_LEN: usize = 512;

/// Bound to the owning user so a ciphertext copied from one user's row
/// into another's can never decrypt.
fn aad(user_id: Uuid) -> Vec<u8> {
    format!("user_clickup_credentials:{user_id}").into_bytes()
}

/// The production client talks to ClickUp's real API. Tests aim it at a
/// local mock through the `CLICKUP_API_BASE_URL` seam; that override is
/// compiled out of release builds so a stray environment variable can
/// never redirect users' tokens to another host.
fn clickup_client(state: &AppState) -> ClickUpClient {
    #[cfg(test)]
    if let Some(base_url) = state.env_source.get(crate::clickup::BASE_URL_ENV) {
        return ClickUpClient::new(&base_url);
    }

    let _ = state;
    ClickUpClient::new(crate::clickup::DEFAULT_BASE_URL)
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    /// No token saved for this user.
    NotConnected,
    /// A token is saved and ClickUp accepted it when last checked.
    Connected,
    /// A token is saved but ClickUp rejected it on the last check.
    Invalid,
}

#[derive(Debug, Serialize)]
pub struct ConnectionResponse {
    pub status: ConnectionStatus,
    pub clickup_user_id: Option<String>,
    pub clickup_username: Option<String>,
    pub last_validated_at: Option<DateTime<Utc>>,
    /// Workspace names ClickUp reported on the check this response
    /// follows. Empty on a plain status read, which makes no ClickUp
    /// call.
    pub workspace_names: Vec<String>,
}

impl ConnectionResponse {
    fn not_connected() -> Self {
        Self {
            status: ConnectionStatus::NotConnected,
            clickup_user_id: None,
            clickup_username: None,
            last_validated_at: None,
            workspace_names: Vec::new(),
        }
    }
}

#[derive(sqlx::FromRow)]
struct StatusRow {
    status: String,
    clickup_user_id: Option<String>,
    clickup_username: Option<String>,
    last_validated_at: Option<DateTime<Utc>>,
}

impl StatusRow {
    fn into_response(self, workspace_names: Vec<String>) -> ConnectionResponse {
        ConnectionResponse {
            status: if self.status == "valid" {
                ConnectionStatus::Connected
            } else {
                ConnectionStatus::Invalid
            },
            clickup_user_id: self.clickup_user_id,
            clickup_username: self.clickup_username,
            last_validated_at: self.last_validated_at,
            workspace_names,
        }
    }
}

const STATUS_COLUMNS: &str = "status, clickup_user_id, clickup_username, last_validated_at";

/// 502 for "ClickUp itself could not be reached / misbehaved" -- the
/// user's token may be perfectly fine, so this is not a 4xx.
fn clickup_unavailable(err: &ClickUpError) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(ApiErrorBody {
            error: "clickup_unavailable",
            message: format!("Could not complete the request to ClickUp: {err}"),
        }),
    )
        .into_response()
}

pub async fn get_connection(State(state): State<AppState>, user: AuthenticatedUser) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "get_clickup_connection", None, None)
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for ClickUp connection read");
            return internal_error("Could not load your ClickUp connection");
        }
    };

    let row: Result<Option<StatusRow>, sqlx::Error> = sqlx::query_as(&format!(
        "SELECT {STATUS_COLUMNS} FROM integrations.user_clickup_credentials WHERE user_id = $1"
    ))
    .bind(user.user_id)
    .fetch_optional(&mut *tx)
    .await;

    let row = match row {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "ClickUp connection read failed");
            return internal_error("Could not load your ClickUp connection");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit ClickUp connection read");
        return internal_error("Could not load your ClickUp connection");
    }

    Json(match row {
        Some(row) => row.into_response(Vec::new()),
        None => ConnectionResponse::not_connected(),
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct SaveTokenRequest {
    pub token: String,
}

pub async fn save_token(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<SaveTokenRequest>,
) -> Response {
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    if let Err(response) = user
        .require_permission(
            &state.db,
            PERMISSION,
            "save_clickup_token",
            user_agent,
            ip_address,
        )
        .await
    {
        return response;
    }

    let token = request.token.trim();

    if token.is_empty() || token.len() > MAX_TOKEN_LEN || token.chars().any(char::is_whitespace) {
        return bad_request(
            "invalid_clickup_token",
            "Paste your ClickUp personal API token exactly as ClickUp shows it (it contains no spaces).".to_string(),
        );
    }

    let identity: ClickUpIdentity = match clickup_client(&state).identify(token).await {
        Ok(identity) => identity,
        Err(ClickUpError::Unauthorized) => {
            return bad_request(
                "invalid_clickup_token",
                "ClickUp rejected this token. Check that you copied the whole token and that it has not been regenerated.".to_string(),
            );
        }
        Err(err) => {
            tracing::warn!(error = %err, user_id = %user.user_id, "ClickUp unreachable while validating a new token");
            return clickup_unavailable(&err);
        }
    };

    let ciphertext = match secrets::encrypt(&aad(user.user_id), token) {
        Ok(blob) => blob,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to encrypt ClickUp token");
            return internal_error("Could not save your ClickUp token");
        }
    };

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for ClickUp token save");
            return internal_error("Could not save your ClickUp token");
        }
    };

    let row: Result<StatusRow, sqlx::Error> = sqlx::query_as(&format!(
        "INSERT INTO integrations.user_clickup_credentials
             (user_id, token_ciphertext, clickup_user_id, clickup_username, status, last_validated_at)
         VALUES ($1, $2, $3, $4, 'valid', now())
         ON CONFLICT (user_id) DO UPDATE
            SET token_ciphertext = EXCLUDED.token_ciphertext,
                clickup_user_id = EXCLUDED.clickup_user_id,
                clickup_username = EXCLUDED.clickup_username,
                status = 'valid',
                last_validated_at = now()
         RETURNING {STATUS_COLUMNS}"
    ))
    .bind(user.user_id)
    .bind(&ciphertext)
    .bind(&identity.user_id)
    .bind(&identity.username)
    .fetch_one(&mut *tx)
    .await;

    let row = match row {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "ClickUp token save failed");
            if let Err(rollback_err) = tx.rollback().await {
                tracing::error!(error = %rollback_err, "failed to roll back a failed ClickUp token save");
            }
            return internal_error("Could not save your ClickUp token");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit ClickUp token save");
        return internal_error("Could not save your ClickUp token");
    }

    audit_log::record(
        &state.db,
        audit_log::event::INTEGRATION_CONNECTED,
        audit_log::Subjects::by(user.user_id),
        user_agent,
        ip_address,
        audit_log::Change::none(),
        serde_json::json!({ "integration": "clickup", "clickup_user_id": identity.user_id }),
    )
    .await;

    tracing::info!(user_id = %user.user_id, "ClickUp token saved");

    Json(row.into_response(identity.workspace_names)).into_response()
}

/// Re-checks the saved token against ClickUp now. A rejection flips the
/// stored status to `invalid` (so the nav dot goes red until the user
/// pastes a new token); an unreachable ClickUp leaves it untouched.
pub async fn test_connection(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    if let Err(response) = user
        .require_permission(
            &state.db,
            PERMISSION,
            "test_clickup_connection",
            user_agent,
            ip_address,
        )
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for ClickUp connection test");
            return internal_error("Could not test your ClickUp connection");
        }
    };

    let stored: Result<Option<Vec<u8>>, sqlx::Error> = sqlx::query_scalar(
        "SELECT token_ciphertext FROM integrations.user_clickup_credentials WHERE user_id = $1",
    )
    .bind(user.user_id)
    .fetch_optional(&mut *tx)
    .await;

    let ciphertext = match stored {
        Ok(Some(blob)) => blob,
        Ok(None) => {
            if let Err(err) = tx.rollback().await {
                tracing::error!(error = %err, "failed to roll back a ClickUp test with nothing stored");
            }
            return crate::api::not_found(
                "clickup_not_connected",
                "You have not saved a ClickUp token yet.".to_string(),
            );
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "ClickUp token lookup failed");
            return internal_error("Could not test your ClickUp connection");
        }
    };

    let token = match secrets::decrypt(&aad(user.user_id), &ciphertext) {
        Ok(token) => token,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to decrypt stored ClickUp token");
            return internal_error("Could not test your ClickUp connection");
        }
    };

    // The transaction is only held for the read above; ClickUp is not
    // called while a connection from the pool is checked out.
    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit ClickUp token lookup");
        return internal_error("Could not test your ClickUp connection");
    }

    let outcome = clickup_client(&state).identify(&token).await;

    let identity = match outcome {
        Ok(identity) => Some(identity),
        Err(ClickUpError::Unauthorized) => None,
        Err(err) => {
            tracing::warn!(error = %err, user_id = %user.user_id, "ClickUp unreachable while testing a stored token");
            return clickup_unavailable(&err);
        }
    };

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction to record a ClickUp test result");
            return internal_error("Could not test your ClickUp connection");
        }
    };

    // A rejected token keeps the identity ClickUp last reported for it
    // (so the page can still say whose token went bad); only the status
    // and timestamp change.
    let row: Result<StatusRow, sqlx::Error> = match &identity {
        Some(identity) => {
            sqlx::query_as(&format!(
                "UPDATE integrations.user_clickup_credentials
                    SET status = 'valid', clickup_user_id = $2, clickup_username = $3, last_validated_at = now()
                  WHERE user_id = $1
              RETURNING {STATUS_COLUMNS}"
            ))
            .bind(user.user_id)
            .bind(&identity.user_id)
            .bind(&identity.username)
            .fetch_one(&mut *tx)
            .await
        }
        None => {
            sqlx::query_as(&format!(
                "UPDATE integrations.user_clickup_credentials
                    SET status = 'invalid', last_validated_at = now()
                  WHERE user_id = $1
              RETURNING {STATUS_COLUMNS}"
            ))
            .bind(user.user_id)
            .fetch_one(&mut *tx)
            .await
        }
    };

    let row = match row {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to record a ClickUp test result");
            if let Err(rollback_err) = tx.rollback().await {
                tracing::error!(error = %rollback_err, "failed to roll back a failed ClickUp test update");
            }
            return internal_error("Could not test your ClickUp connection");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit a ClickUp test result");
        return internal_error("Could not test your ClickUp connection");
    }

    Json(row.into_response(identity.map(|i| i.workspace_names).unwrap_or_default())).into_response()
}

/// Removes the caller's saved token. Idempotent: removing when nothing
/// is saved is not an error.
pub async fn remove_token(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    if let Err(response) = user
        .require_permission(
            &state.db,
            PERMISSION,
            "remove_clickup_token",
            user_agent,
            ip_address,
        )
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for ClickUp token removal");
            return internal_error("Could not remove your ClickUp token");
        }
    };

    let deleted =
        sqlx::query("DELETE FROM integrations.user_clickup_credentials WHERE user_id = $1")
            .bind(user.user_id)
            .execute(&mut *tx)
            .await;

    let deleted = match deleted {
        Ok(result) => result.rows_affected(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "ClickUp token removal failed");
            if let Err(rollback_err) = tx.rollback().await {
                tracing::error!(error = %rollback_err, "failed to roll back a failed ClickUp token removal");
            }
            return internal_error("Could not remove your ClickUp token");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit ClickUp token removal");
        return internal_error("Could not remove your ClickUp token");
    }

    if deleted > 0 {
        audit_log::record(
            &state.db,
            audit_log::event::INTEGRATION_DISCONNECTED,
            audit_log::Subjects::by(user.user_id),
            user_agent,
            ip_address,
            audit_log::Change::none(),
            serde_json::json!({ "integration": "clickup" }),
        )
        .await;

        tracing::info!(user_id = %user.user_id, "ClickUp token removed");
    }

    Json(ConnectionResponse::not_connected()).into_response()
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::{clickup_user, empty_state, test_user};

    fn local_addr() -> ConnectInfo<SocketAddr> {
        ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    #[tokio::test]
    async fn get_refuses_a_caller_without_the_permission() {
        let response = get_connection(State(empty_state()), test_user()).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn save_refuses_a_caller_without_the_permission() {
        let response = save_token(
            State(empty_state()),
            test_user(),
            local_addr(),
            HeaderMap::new(),
            Json(SaveTokenRequest {
                token: "pk_whatever".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_refuses_a_caller_without_the_permission() {
        let response = test_connection(
            State(empty_state()),
            test_user(),
            local_addr(),
            HeaderMap::new(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn remove_refuses_a_caller_without_the_permission() {
        let response = remove_token(
            State(empty_state()),
            test_user(),
            local_addr(),
            HeaderMap::new(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn get_with_the_permission_gets_past_the_gate_and_reaches_the_database() {
        // The disconnected test pool then fails: 500, not 403.
        let response = get_connection(State(empty_state()), clickup_user()).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn save_rejects_an_empty_token_before_calling_clickup() {
        let response = save_token(
            State(empty_state()),
            clickup_user(),
            local_addr(),
            HeaderMap::new(),
            Json(SaveTokenRequest {
                token: "   ".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_rejects_a_token_containing_whitespace() {
        let response = save_token(
            State(empty_state()),
            clickup_user(),
            local_addr(),
            HeaderMap::new(),
            Json(SaveTokenRequest {
                token: "pk_abc def".to_string(),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn save_rejects_an_oversized_token() {
        let response = save_token(
            State(empty_state()),
            clickup_user(),
            local_addr(),
            HeaderMap::new(),
            Json(SaveTokenRequest {
                token: "x".repeat(MAX_TOKEN_LEN + 1),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn the_aad_is_bound_to_the_owning_user() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        assert_ne!(aad(a), aad(b));
        assert_eq!(aad(a), aad(a));
    }

    #[test]
    #[serial_test::serial(integration_secrets_encryption_key_env)]
    fn a_ciphertext_does_not_decrypt_under_another_users_aad() {
        std::env::set_var(
            "INTEGRATION_SECRETS_ENCRYPTION_KEY",
            "0000000000000000000000000000000000000000000000000000000000000000",
        );

        let owner = Uuid::new_v4();
        let someone_else = Uuid::new_v4();
        let blob = secrets::encrypt(&aad(owner), "pk_secret").unwrap();

        assert_eq!(secrets::decrypt(&aad(owner), &blob).unwrap(), "pk_secret");
        assert!(secrets::decrypt(&aad(someone_else), &blob).is_err());

        std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
    }
}
