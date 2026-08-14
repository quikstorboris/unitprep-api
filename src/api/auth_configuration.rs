//! Org-wide auth policy (Administration > Security Policies), admin-only
//! read and write.
//!
//! Deliberately scoped to `step_up_actions` only, the one column this
//! table has that any code path actually reads
//! (`step_up_policy::action_requires_step_up`). `allowed_factors` exists
//! in the schema too but nothing currently enforces it anywhere -- no
//! login or fallback-factor path checks it -- so a UI to edit it would
//! let an admin toggle something with zero effect on real behaviour.
//! Left out until it's actually wired to something, not silently
//! forgotten (see THREAT_MODEL.md / the vault for the open item).
//!
//! Session-length and passkey-count-limit policies, also once discussed,
//! have no backing columns at all today (they're env vars, or don't
//! exist as a concept) -- a real follow-up, not part of this page.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{audit_log, begin_rls_transaction, AuthenticatedUser, ADD_PASSKEY};

#[derive(Debug, Serialize)]
pub struct AuthConfigurationResponse {
    pub step_up_actions: Vec<String>,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAuthConfigurationRequest {
    pub step_up_actions: Vec<String>,
}

/// Every action name `step_up_actions` is allowed to contain -- just the
/// one that exists today. Rejecting anything else here is what stands
/// between a client-side typo and a JSONB array silently accumulating
/// dead entries that `action_requires_step_up` will never match against.
const KNOWN_STEP_UP_ACTIONS: &[&str] = &[ADD_PASSKEY];

fn bad_request(error: &'static str, message: String) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody { error, message }),
    )
        .into_response()
}

pub async fn get_configuration(
    State(state): State<AppState>,
    admin: AuthenticatedUser,
) -> Response {
    if let Err(response) = admin
        .require_permission(
            &state.db,
            "security_policies.manage",
            "get_configuration",
            None,
            None,
        )
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, &admin.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for configuration read");
            return internal_error("Could not load security policies");
        }
    };

    #[allow(clippy::type_complexity)]
    let row: Result<
        (sqlx::types::Json<Vec<String>>, DateTime<Utc>, Option<Uuid>),
        sqlx::Error,
    > = sqlx::query_as(
        "SELECT step_up_actions, updated_at, updated_by FROM auth.auth_configuration WHERE id = 1",
    )
    .fetch_one(&mut *tx)
    .await;

    let (step_up_actions, updated_at, updated_by) = match row {
        Ok((step_up_actions, updated_at, updated_by)) => {
            (step_up_actions.0, updated_at, updated_by)
        }
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "configuration read query failed");
            return internal_error("Could not load security policies");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to commit configuration read transaction");
        return internal_error("Could not load security policies");
    }

    Json(AuthConfigurationResponse {
        step_up_actions,
        updated_at,
        updated_by,
    })
    .into_response()
}

pub async fn update_configuration(
    State(state): State<AppState>,
    admin: AuthenticatedUser,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<UpdateAuthConfigurationRequest>,
) -> Response {
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    if let Err(response) = admin
        .require_permission(
            &state.db,
            "security_policies.manage",
            "update_configuration",
            user_agent,
            ip_address,
        )
        .await
    {
        return response;
    }

    if let Some(unknown) = request
        .step_up_actions
        .iter()
        .find(|action| !KNOWN_STEP_UP_ACTIONS.contains(&action.as_str()))
    {
        return bad_request(
            "invalid_step_up_action",
            format!("\"{unknown}\" is not a recognised step-up action."),
        );
    }

    let mut tx = match begin_rls_transaction(&state.db, admin.user_id, &admin.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to open transaction for configuration update");
            return internal_error("Could not update security policies");
        }
    };

    let prior: Result<sqlx::types::Json<Vec<String>>, sqlx::Error> =
        sqlx::query_scalar("SELECT step_up_actions FROM auth.auth_configuration WHERE id = 1")
            .fetch_one(&mut *tx)
            .await;

    let prior_actions = match prior {
        Ok(actions) => actions.0,
        Err(err) => {
            tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to read prior configuration during update");
            return internal_error("Could not update security policies");
        }
    };

    if let Err(err) = sqlx::query(
        "UPDATE auth.auth_configuration
            SET step_up_actions = $1, updated_by = $2
          WHERE id = 1",
    )
    .bind(sqlx::types::Json(&request.step_up_actions))
    .bind(admin.user_id)
    .execute(&mut *tx)
    .await
    {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, "configuration update failed");
        return internal_error("Could not update security policies");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, admin_user_id = %admin.user_id, "failed to commit configuration update");
        return internal_error("Could not update security policies");
    }

    audit_log::record(
        &state.db,
        audit_log::event::AUTH_CONFIGURATION_UPDATED,
        audit_log::Subjects::by(admin.user_id),
        user_agent,
        ip_address,
        audit_log::Change::from_to(
            serde_json::json!({ "step_up_actions": prior_actions }),
            serde_json::json!({ "step_up_actions": request.step_up_actions }),
        ),
        serde_json::json!({}),
    )
    .await;

    tracing::info!(
        admin_user_id = %admin.user_id,
        "security policy updated"
    );

    Json(serde_json::json!({ "step_up_actions": request.step_up_actions })).into_response()
}
