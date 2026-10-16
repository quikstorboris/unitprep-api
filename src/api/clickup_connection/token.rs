//! Saving (and replacing) the personal API token, and removing it.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Json, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};

use super::{
    aad, clickup_client, clickup_unavailable, ConnectionResponse, SaveTokenRequest, StatusRow,
    MAX_TOKEN_LEN, PERMISSION, STATUS_COLUMNS,
};
use crate::api::rls::{begin_for, try_response};
use crate::api::{bad_request, internal_error, AppState};
use crate::auth::{audit_log, AuthenticatedUser};
use crate::clickup::{ClickUpError, ClickUpIdentity};
use crate::integrations::secrets;

pub async fn save_token(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<SaveTokenRequest>,
) -> Response {
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    try_response!(
        user.require_permission(
            &state.db,
            PERMISSION,
            "save_clickup_token",
            user_agent,
            ip_address,
        )
        .await
    );

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

    let mut tx = try_response!(begin_for(&state, &user, "Could not save your ClickUp token").await);

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

    // A different token may see a different slice of the workspace.
    crate::clickup::hierarchy::invalidate(user.user_id);

    tracing::info!(user_id = %user.user_id, "ClickUp token saved");

    Json(row.into_response(identity.workspace_names)).into_response()
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

    try_response!(
        user.require_permission(
            &state.db,
            PERMISSION,
            "remove_clickup_token",
            user_agent,
            ip_address,
        )
        .await
    );

    let mut tx =
        try_response!(begin_for(&state, &user, "Could not remove your ClickUp token").await);

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

    crate::clickup::hierarchy::invalidate(user.user_id);

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
