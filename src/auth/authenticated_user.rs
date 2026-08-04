use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use axum_extra::extract::cookie::CookieJar;
use chrono::{DateTime, Utc};
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
    /// This exact session's own token hash -- needed by anything that acts
    /// on "the session this request came in on" specifically, e.g.
    /// recording a step-up verification (auth.record_step_up), which must
    /// elevate only the one device that just proved a fresh TOTP code, not
    /// every session this user has open. Never logged or returned to a
    /// client -- same handling as everywhere else this hash appears.
    pub token_hash: Vec<u8>,
    /// None if this session has never been step-up verified, or if a past
    /// verification has expired -- both mean "not currently elevated" to
    /// every caller, so nothing downstream needs to tell them apart. See
    /// `is_elevated`.
    pub elevated_until: Option<DateTime<Utc>>,
}

impl AuthenticatedUser {
    pub fn is_elevated(&self) -> bool {
        self.elevated_until.is_some_and(|until| until > Utc::now())
    }
}

/// How long a session may go without an authenticated request before it is
/// treated as expired, independent of `auth.sessions.expires_at`'s
/// absolute ceiling.
///
/// This is the idle half of the Phase II session-hardening pair: the
/// absolute expiry (`SESSION_LIFETIME_HOURS`, set once at login and never
/// extended) already bounds how long a session can exist at all; this
/// bounds how long it can exist *unused*. A session left open and
/// unattended for the rest of a 12-hour window was, before this, fully
/// valid the whole time -- `resolve_session` bumps `last_seen_at` on every
/// authenticated request, but nothing read it back to enforce a limit.
///
/// Same override-and-default shape as `session_lifetime_hours` in
/// auth_login.rs, and the same reasoning for a floor of 1 rather than
/// letting a misconfigured 0/negative value be read as "no idle timeout" --
/// an idle timeout that can be silently disabled by a typo is not a
/// hardening control.
fn session_idle_timeout_minutes() -> i32 {
    std::env::var("SESSION_IDLE_TIMEOUT_MINUTES")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|minutes| *minutes > 0)
        .unwrap_or(30)
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

        let Some(raw_token) = read_session_cookie(&jar) else {
            return Err(unauthorized());
        };

        let token_hash = hash_token(&raw_token);

        let row = query_session(&token_hash, &state.db)
            .await
            .map_err(|_| internal_error())?;

        let (user_id, role_text, elevated_until) = row.ok_or_else(unauthorized)?;

        let role = Role::from_db_text(&role_text).ok_or_else(internal_error)?;

        Ok(AuthenticatedUser {
            user_id,
            role,
            token_hash,
            elevated_until,
        })
    }
}

/// The one query behind session resolution -- shared by the mandatory
/// extractor above and by `try_authenticated_user` below so there is
/// exactly one place that knows resolve_session's shape.
async fn query_session(
    token_hash: &[u8],
    db: &PgPool,
) -> Result<Option<(Uuid, String, Option<DateTime<Utc>>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT user_id, role::text, elevated_until FROM auth.resolve_session($1, $2)",
    )
    .bind(token_hash)
    .bind(session_idle_timeout_minutes())
    .fetch_optional(db)
    .await
}

/// A best-effort version of the extractor above, for a handler that
/// needs to know "is there already a valid session" without rejecting
/// the request when there isn't one -- see register_begin, which treats
/// an existing session as "add another passkey for yourself" and an
/// absent or unresolvable one as the bootstrap path instead of a 401.
/// Any failure (missing cookie, DB error, unresolved session, unknown
/// role) collapses to `None` here -- unlike the mandatory extractor,
/// nothing downstream needs to tell those cases apart, since the caller
/// falls through to its own, entirely separate bootstrap checks either
/// way.
pub async fn try_authenticated_user(
    jar: &CookieJar,
    state: &AppState,
) -> Option<AuthenticatedUser> {
    let raw_token = read_session_cookie(jar)?;
    let token_hash = hash_token(&raw_token);
    let (user_id, role_text, elevated_until) =
        query_session(&token_hash, &state.db).await.ok()??;
    let role = Role::from_db_text(&role_text)?;

    Some(AuthenticatedUser {
        user_id,
        role,
        token_hash,
        elevated_until,
    })
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

/// Shared with any handler gating part of itself behind
/// `AuthenticatedUser::is_elevated` -- e.g. `register_begin`'s
/// authenticated add-a-passkey branch (see auth_register.rs).
pub fn step_up_required() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ApiErrorBody {
            error: "step_up_required",
            message: "Enter your authenticator app code to confirm this action.".to_string(),
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

    sqlx::query("SELECT set_config('app.current_user_id', $1, true)")
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await?;

    sqlx::query("SELECT set_config('app.current_user_role', $1, true)")
        .bind(role.as_db_text())
        .execute(&mut *tx)
        .await?;

    Ok(tx)
}

/// Like `begin_rls_transaction`, but sets ONLY `app.current_user_id` and
/// deliberately leaves `app.current_user_role` unset.
///
/// For pre-authentication flows that must write a row owned by a
/// specific user before that user has any established role to assert --
/// today, the bootstrap half of passkey registration (see
/// `api::auth_register`), where no session exists yet by definition.
///
/// Strictly LESS privilege than `begin_rls_transaction`, not a
/// convenience variant: every admin-bypass branch in this schema's
/// policies is written as `current_setting('app.current_user_role', true)
/// = 'admin'`, which safely evaluates false when the setting is unset
/// (a text comparison, so it needs no `NULLIF` guard -- unlike the uuid
/// casts, see the RLS notes on why those do). So a transaction opened
/// here can reach owner-scoped rows and nothing else, and cannot
/// accidentally inherit admin visibility.
pub async fn begin_owner_rls_transaction(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<sqlx::Transaction<'_, sqlx::Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query("SELECT set_config('app.current_user_id', $1, true)")
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await?;

    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_round_trips_through_its_db_text_form() {
        assert_eq!(
            Role::from_db_text(Role::Admin.as_db_text()),
            Some(Role::Admin)
        );
    }

    #[test]
    fn unknown_db_text_does_not_match_any_role() {
        assert_eq!(Role::from_db_text("not_a_real_role"), None);
    }

    fn user_with_elevation(elevated_until: Option<DateTime<Utc>>) -> AuthenticatedUser {
        AuthenticatedUser {
            user_id: Uuid::new_v4(),
            role: Role::Admin,
            token_hash: vec![0u8; 32],
            elevated_until,
        }
    }

    #[test]
    fn never_stepped_up_is_not_elevated() {
        assert!(!user_with_elevation(None).is_elevated());
    }

    #[test]
    fn a_future_elevation_deadline_is_elevated() {
        let deadline = Utc::now() + chrono::Duration::minutes(5);
        assert!(user_with_elevation(Some(deadline)).is_elevated());
    }

    #[test]
    fn a_past_elevation_deadline_is_not_elevated() {
        let deadline = Utc::now() - chrono::Duration::seconds(1);
        assert!(!user_with_elevation(Some(deadline)).is_elevated());
    }
}
