//! Read-only role/permission catalog, for the admin Roles page and for
//! any future UI that needs to know what a role can do (e.g. a role
//! picker). No permission gate -- `auth.roles`/`auth.role_permissions`
//! are catalog data any authenticated caller can already read under RLS
//! (`roles_select_authenticated`), and there's nothing sensitive in a
//! role's name or its permission list.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use crate::api::{internal_error, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};

#[derive(Debug, Serialize)]
pub struct RoleInfo {
    pub key: String,
    pub label: String,
    pub description: Option<String>,
    pub is_system: bool,
    pub permissions: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct ListRolesResponse {
    pub roles: Vec<RoleInfo>,
}

pub async fn list_roles(State(state): State<AppState>, user: AuthenticatedUser) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for role listing");
            return internal_error("Could not list roles");
        }
    };

    #[allow(clippy::type_complexity)]
    let rows: Result<Vec<(String, String, Option<String>, bool, Vec<String>)>, sqlx::Error> =
        sqlx::query_as(
            "SELECT r.key, r.label, r.description, r.is_system,
                COALESCE(
                    array_agg(rp.permission_key ORDER BY rp.permission_key)
                        FILTER (WHERE rp.permission_key IS NOT NULL),
                    '{}'
                )
           FROM auth.roles r
           LEFT JOIN auth.role_permissions rp ON rp.role_id = r.id
          GROUP BY r.id, r.key, r.label, r.description, r.is_system
          ORDER BY r.key",
        )
        .fetch_all(&mut *tx)
        .await;

    let rows = match rows {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "role listing query failed");
            return internal_error("Could not list roles");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit role listing transaction");
        return internal_error("Could not list roles");
    }

    let roles = rows
        .into_iter()
        .map(
            |(key, label, description, is_system, permissions)| RoleInfo {
                key,
                label,
                description,
                is_system,
                permissions,
            },
        )
        .collect();

    Json(ListRolesResponse { roles }).into_response()
}
