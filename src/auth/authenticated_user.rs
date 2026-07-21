use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::extract::cookie::CookieJar;
use sqlx::PgPool;
use uuid::Uuid;

use crate::api::{ApiErrorBody, AppState};

use super::{hash_token, read_session_cookie};

/// The one role that exists in v1 -- an enum with a single variant,
/// not a bare String, so adding the other three roles later (per the
/// architecture doc's extensible-role-column design) means adding a
/// variant, not restructuring every call site that matches on a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
}

impl Role {
    fn from_db_text(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }

    pub fn as_db_text(&self) -> &'static str {
        match self {
            Role::Admin => "admin",
        }
    }
}

/// Extracted once per request from the session cookie. Resolves the
/// session via resolve_session() -- a SECURITY DEFINER function that
/// bypasses RLS on its own, so no GUC context is needed for this one
/// lookup. Handlers that go on to run further RLS-scoped queries under
/// this identity must use begin_rls_transaction below rather than
/// assume any GUC is already set on the shared pool -- pooled
/// connections are not identity-scoped between requests.
pub struct AuthenticatedUser {
    pub user_id: Uuid,
    pub role: Role,
}

impl FromRequestParts<AppState> for AuthenticatedUser {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_request_parts(parts, state)
            .await
            .expect("CookieJar extraction is infallible");

        let raw_token =
            read_session_cookie(&jar).ok_or_else(unauthorized)?;
        let token_hash = hash_token(&raw_token);

        let row: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT user_id, role::text FROM resolve_session($1)",
        )
        .bind(token_hash)
        .fetch_optional(&state.db)
        .await
        .map_err(|_| internal_error())?;

        let (user_id, role_text) =
            row.ok_or_else(unauthorized)?;

        let role = Role::from_db_text(&role_text)
            .ok_or_else(internal_error)?;

        Ok(AuthenticatedUser { user_id, role })
    }
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ApiErrorBody {
            error: "unauthorized",
            message: "Sign in required".to_string(),
        }),
    )
        .into_response()
}

fn internal_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ApiErrorBody {
            error: "internal_error",
            message: "Failed to verify session -- check server logs for details.".to_string(),
        }),
    )
        .into_response()
}

/// Begins a transaction and sets the per-request RLS GUCs on it via
/// set_configs third (is_local) argument -- equivalent to SET LOCAL,
/// scoped to this transaction only and automatically reset on commit
/// or rollback, so a pooled connection can never leak one requests
/// identity into a later, unrelated request that happens to reuse it.
/// Callers run their RLS-scoped queries against the returned
/// transaction and commit it themselves.
pub async fn begin_rls_transaction(
    pool: &PgPool,
    user_id: Uuid,
    role: Role,
) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query(
        "SELECT set_config('app.current_user_id', $1, true)",
    )
    .bind(user_id.to_string())
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "SELECT set_config('app.current_user_role', $1, true)",
    )
    .bind(role.as_db_text())
    .execute(&mut *tx)
    .await?;

    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_round_trips_through_its_db_text_form() {
        assert_eq!(Role::from_db_text(Role::Admin.as_db_text()), Some(Role::Admin));
    }

    #[test]
    fn unknown_db_text_does_not_match_any_role() {
        assert_eq!(Role::from_db_text("not_a_real_role"), None);
    }
}
