//! Reading the connection's status and re-testing the saved token.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Json, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};

use super::{
    aad, clickup_client, clickup_unavailable, ConnectionResponse, StatusRow, PERMISSION,
    STATUS_COLUMNS,
};
use crate::api::rls::{begin_for, try_response};
use crate::api::{internal_error, AppState};
use crate::auth::AuthenticatedUser;
use crate::clickup::ClickUpError;
use crate::integrations::secrets;

pub async fn get_connection(State(state): State<AppState>, user: AuthenticatedUser) -> Response {
    try_response!(
        user.require_permission(&state.db, PERMISSION, "get_clickup_connection", None, None)
            .await
    );

    let mut tx =
        try_response!(begin_for(&state, &user, "Could not load your ClickUp connection").await);

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

    try_response!(
        user.require_permission(
            &state.db,
            PERMISSION,
            "test_clickup_connection",
            user_agent,
            ip_address,
        )
        .await
    );

    let mut tx =
        try_response!(begin_for(&state, &user, "Could not test your ClickUp connection").await);

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

    let mut tx =
        try_response!(begin_for(&state, &user, "Could not test your ClickUp connection").await);

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
