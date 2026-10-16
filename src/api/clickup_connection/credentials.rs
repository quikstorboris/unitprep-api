//! Reading the calling user's saved ClickUp token for the endpoints that
//! call ClickUp on their behalf, and mapping a failed ClickUp call to the
//! response to return.

use axum::{http::StatusCode, response::Response};

use super::aad;
use crate::api::{conflict, error_response, internal_error, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clickup::ClickUpError;
use crate::integrations::secrets;

/// 502 for "ClickUp itself could not be reached / misbehaved" -- the
/// user's token may be perfectly fine, so this is not a 4xx.
pub(crate) fn clickup_unavailable(err: &ClickUpError) -> Response {
    error_response(
        StatusCode::BAD_GATEWAY,
        "clickup_unavailable",
        format!("Could not complete the request to ClickUp: {err}"),
    )
}

/// The calling user's decrypted ClickUp token, for the endpoints that
/// make ClickUp calls on their behalf (link suggestions, resolving a
/// pasted URL, confirming a link). `Err` is the response to return as-is:
/// 409 when they have not connected ClickUp, or their saved token was
/// already found to be invalid -- both fixable on the ClickUp page, and
/// distinct from a server fault.
pub(crate) async fn load_user_token(
    state: &AppState,
    user: &AuthenticatedUser,
) -> Result<String, Response> {
    load_user_credentials(state, user)
        .await
        .map(|(token, _)| token)
}

/// The caller's decrypted token plus their ClickUp user id (when ClickUp
/// reported one), read in a single database round trip -- for endpoints
/// that need both (adding the user as an assignee).
pub(crate) async fn load_user_credentials(
    state: &AppState,
    user: &AuthenticatedUser,
) -> Result<(String, Option<String>), Response> {
    let mut tx = begin_rls_transaction(&state.db, user.user_id, &user.role_keys)
        .await
        .map_err(|err| {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction to load a ClickUp token");
            internal_error("Could not use your ClickUp connection")
        })?;

    let row: Option<(Vec<u8>, String, Option<String>)> = sqlx::query_as(
        "SELECT token_ciphertext, status, clickup_user_id FROM integrations.user_clickup_credentials WHERE user_id = $1",
    )
    .bind(user.user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|err| {
        tracing::error!(error = %err, user_id = %user.user_id, "ClickUp token lookup failed");
        internal_error("Could not use your ClickUp connection")
    })?;

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit ClickUp token lookup");
        return Err(internal_error("Could not use your ClickUp connection"));
    }

    let Some((ciphertext, status, clickup_user_id)) = row else {
        return Err(conflict(
            "clickup_not_connected",
            "Connect your ClickUp account first (My Integrations > ClickUp).".to_string(),
        ));
    };

    if status != "valid" {
        return Err(conflict(
            "clickup_token_invalid",
            "ClickUp rejected your saved token. Paste a new one under My Integrations > ClickUp."
                .to_string(),
        ));
    }

    secrets::decrypt(&aad(user.user_id), &ciphertext)
        .map(|token| (token, clickup_user_id))
        .map_err(|err| {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to decrypt stored ClickUp token");
            internal_error("Could not use your ClickUp connection")
        })
}

/// Maps a failed ClickUp call made *with a token we believed valid* to
/// the response the caller should return. A rejected token also flips the
/// stored status to `invalid`, so the nav dot turns red and the next call
/// short-circuits with the actionable 409 instead of hitting ClickUp
/// again. `NotFound` is left to the caller, which knows what was being
/// looked up.
pub(crate) async fn clickup_failure_response(
    state: &AppState,
    user: &AuthenticatedUser,
    err: &ClickUpError,
) -> Response {
    match err {
        ClickUpError::Unauthorized => {
            if let Ok(mut tx) =
                begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await
            {
                let _ = sqlx::query(
                    "UPDATE integrations.user_clickup_credentials
                        SET status = 'invalid', last_validated_at = now()
                      WHERE user_id = $1",
                )
                .bind(user.user_id)
                .execute(&mut *tx)
                .await;
                let _ = tx.commit().await;
            }
            conflict(
                "clickup_token_invalid",
                "ClickUp rejected your saved token. Paste a new one under My Integrations > ClickUp.".to_string(),
            )
        }
        other => clickup_unavailable(other),
    }
}
