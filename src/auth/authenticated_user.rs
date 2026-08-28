use std::collections::HashSet;

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
use crate::auth::audit_log;

use super::{hash_token, read_session_cookie};

/// Extracted once per request from the session cookie. Resolves the
/// session via resolve_session() -- a SECURITY DEFINER function that
/// bypasses RLS on its own, so no GUC context is needed for this one
/// lookup. Handlers that go on to run further RLS-scoped queries under
/// this identity must use begin_rls_transaction below rather than
/// assume any GUC is already set on the shared pool -- pooled
/// connections are not identity-scoped between requests.
pub struct AuthenticatedUser {
    pub user_id: Uuid,
    /// Every role key this account currently holds -- not a closed Rust
    /// enum. Roles are real data now (four today, more later, eventually
    /// admin-defined custom ones), and hardcoding them in Rust would be
    /// exactly the kind of hardcoding the permission model exists to
    /// avoid. Prefer `has_role`/`has_permission`/`require_permission` to
    /// matching on this directly.
    pub role_keys: Vec<String>,
    /// Every permission key the roles above currently grant, resolved in
    /// the same `resolve_session` call as `role_keys` -- so an
    /// authorization check is a `HashSet` lookup, not a second query.
    pub permission_keys: HashSet<String>,
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
    /// Set at login (Phase II anomaly signal, see auth_login.rs) when this
    /// account has TOTP confirmed and the login looked anomalous -- a new
    /// IP or user_agent for an account with prior sessions to compare
    /// against. While true, the extractor below refuses every route except
    /// the handful needed to clear it; see `STEP_UP_ALLOWED_PATHS`.
    pub requires_step_up: bool,

    /// None if this session has never completed a passkey re-verification,
    /// or if a past one has expired -- both mean "not currently
    /// re-verified" to every caller, same shape as `elevated_until`. A
    /// separate column/field from `elevated_until`, not a reuse of it: the
    /// two answer different questions (proved a TOTP code vs. proved a
    /// passkey assertion) -- see `is_passkey_reverified` and
    /// `auth.record_passkey_reverify`.
    pub passkey_reverified_until: Option<DateTime<Utc>>,
}

impl AuthenticatedUser {
    pub fn is_elevated(&self) -> bool {
        self.elevated_until.is_some_and(|until| until > Utc::now())
    }

    /// Mirrors `is_elevated`, for the passkey-based step-up that gates
    /// self-service TOTP re-enrolment (see auth_passkey_reverify.rs).
    pub fn is_passkey_reverified(&self) -> bool {
        self.passkey_reverified_until
            .is_some_and(|until| until > Utc::now())
    }

    pub fn has_permission(&self, permission_key: &str) -> bool {
        self.permission_keys.contains(permission_key)
    }

    /// Checks `permission_key` and, if the caller lacks it, records an
    /// `AUTHORIZATION_FAILURE` audit row and returns the shared 403 --
    /// replaces what used to be a `match admin.role { Role::Admin => {},
    /// Role::OnboardingManager => { ...403... } }` block duplicated at
    /// every admin-gated call site. Redundant with the RLS layer by
    /// design, same as every check it replaces: this produces a clean 403
    /// with an audit row, and the database's own policies are what hold
    /// if a future handler forgets to call this.
    pub async fn require_permission(
        &self,
        db: &PgPool,
        permission_key: &str,
        action: &'static str,
        user_agent: Option<&str>,
        ip_address: Option<sqlx::types::ipnetwork::IpNetwork>,
    ) -> Result<(), Response> {
        if self.has_permission(permission_key) {
            return Ok(());
        }

        audit_log::record(
            db,
            audit_log::event::AUTHORIZATION_FAILURE,
            audit_log::Subjects::by(self.user_id),
            user_agent,
            ip_address,
            audit_log::Change::none(),
            serde_json::json!({ "action": action, "required_permission": permission_key }),
        )
        .await;

        Err(insufficient_role())
    }
}

/// Routes reachable on a session that still has `requires_step_up = true`.
/// Everything else 403s until the caller clears it. Deliberately a short,
/// explicit allowlist rather than an opt-out per handler -- the gate has to
/// be impossible to forget on a new route, and the alternative (every
/// handler remembering to check `requires_step_up` itself) is exactly the
/// kind of thing that gets missed once and stays missed.
///
/// - `/auth/totp/step-up` -- the only way to actually clear the flag.
/// - `/health/whoami` -- so the frontend can tell "signed in but pending
///   step-up" apart from "not signed in at all" and render the right
///   screen, rather than watching every other route 403 and guessing why.
///
/// `/auth/logout` and `/auth/logout/everywhere` need no entry here: they
/// deliberately don't use `AuthenticatedUser` at all (see auth_logout.rs),
/// so this gate never applies to them regardless.
const STEP_UP_ALLOWED_PATHS: [&str; 2] = ["/auth/totp/step-up", "/health/whoami"];

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

        let row = query_session(&token_hash, &state.db).await.map_err(|err| {
            tracing::error!(error = %err, "session resolution query failed");
            internal_error()
        })?;

        let Some((
            user_id,
            role_keys,
            permission_keys,
            elevated_until,
            requires_step_up,
            passkey_reverified_until,
        )) = row
        else {
            record_expired_access_attempt(state, &token_hash, parts).await;
            return Err(unauthorized());
        };

        // Checked here, in the one place every gated route already passes
        // through, rather than left to each handler -- see
        // STEP_UP_ALLOWED_PATHS's doc comment for why.
        if requires_step_up && !STEP_UP_ALLOWED_PATHS.contains(&parts.uri.path()) {
            return Err(step_up_required());
        }

        Ok(AuthenticatedUser {
            user_id,
            role_keys: role_keys.unwrap_or_default(),
            permission_keys: permission_keys.unwrap_or_default().into_iter().collect(),
            token_hash,
            elevated_until,
            requires_step_up,
            passkey_reverified_until,
        })
    }
}

/// The one query behind session resolution -- shared by the mandatory
/// extractor above and by `try_authenticated_user` below so there is
/// exactly one place that knows resolve_session's shape. `role_keys` and
/// `permission_keys` come back `NULL` (mapped to `None`) rather than an
/// empty array when a user holds no roles at all -- `array_agg` over zero
/// matching rows, not something expected in practice but not a crash
/// either.
#[allow(clippy::type_complexity)]
pub(crate) async fn query_session(
    token_hash: &[u8],
    db: &PgPool,
) -> Result<
    Option<(
        Uuid,
        Option<Vec<String>>,
        Option<Vec<String>>,
        Option<DateTime<Utc>>,
        bool,
        Option<DateTime<Utc>>,
    )>,
    sqlx::Error,
> {
    sqlx::query_as(
        "SELECT user_id, role_keys, permission_keys, elevated_until, requires_step_up, \
         passkey_reverified_until FROM auth.resolve_session($1, $2)",
    )
    .bind(token_hash)
    .bind(session_idle_timeout_minutes())
    .fetch_optional(db)
    .await
}

/// Runs only on the "no valid session" path, to tell an ordinary stale or
/// forged cookie apart from one that named a session which genuinely
/// existed and had crossed its idle or absolute expiry -- only the latter
/// is worth a permanent row. See `auth.check_session_expired`'s own doc
/// comment for why a revoked session doesn't count here: that already has
/// its own `SESSION_REVOKED` event at the time it happened.
///
/// Fire-and-forget like every other audit write -- a failure here must
/// not turn into a different rejection response than an ordinary expired
/// cookie would get.
async fn record_expired_access_attempt(state: &AppState, token_hash: &[u8], parts: &Parts) {
    let expired_user_id: Result<Option<Uuid>, sqlx::Error> =
        sqlx::query_scalar("SELECT user_id FROM auth.check_session_expired($1, $2)")
            .bind(token_hash)
            .bind(session_idle_timeout_minutes())
            .fetch_optional(&state.db)
            .await;

    match expired_user_id {
        Ok(Some(user_id)) => {
            let user_agent = parts
                .headers
                .get(axum::http::header::USER_AGENT)
                .and_then(|value| value.to_str().ok());

            crate::auth::audit_log::record(
                &state.db,
                crate::auth::audit_log::event::SESSION_EXPIRED_ACCESS_ATTEMPT,
                crate::auth::audit_log::Subjects::by(user_id),
                user_agent,
                None,
                crate::auth::audit_log::Change::none(),
                serde_json::json!({ "path": parts.uri.path() }),
            )
            .await;
        }
        Ok(None) => {}
        Err(err) => {
            tracing::error!(error = %err, "expired-session check query failed");
        }
    }
}

/// A best-effort version of the extractor above, for a handler that
/// needs to know "is there already a valid session" without rejecting
/// the request when there isn't one -- see register_begin, which treats
/// an existing session as "add another passkey for yourself" and an
/// absent or unresolvable one as the bootstrap path instead of a 401.
/// Any failure (missing cookie, DB error, unresolved session) collapses
/// to `None` here -- unlike the mandatory extractor, nothing downstream
/// needs to tell those cases apart, since the caller falls through to its
/// own, entirely separate bootstrap checks either way.
///
/// A session pending step-up also collapses to `None` here, deliberately:
/// this function has no request path to consult (unlike the extractor
/// above, which allowlists a couple of specific routes), and the caller
/// this feeds -- "does an authenticated-add-a-passkey path apply?" --
/// should not treat a not-yet-proven-trustworthy session as good enough to
/// silently attach a brand new credential to. Falling through to the
/// bootstrap path is the same outcome an absent session gets, which is
/// correct here.
pub async fn try_authenticated_user(
    jar: &CookieJar,
    state: &AppState,
) -> Option<AuthenticatedUser> {
    let raw_token = read_session_cookie(jar)?;
    let token_hash = hash_token(&raw_token);

    // Unlike the mandatory extractor above, a query failure here used to
    // collapse into the same `None` as "no session" via `.ok()`, silently
    // discarding the error -- a DB outage on this path looked identical to
    // an anonymous caller, with zero trace of the real cause. Logged now,
    // matching the mandatory extractor's own pattern; the None-collapsing
    // behavior itself (this function's own doc comment above) is unchanged.
    let (
        user_id,
        role_keys,
        permission_keys,
        elevated_until,
        requires_step_up,
        passkey_reverified_until,
    ) = match query_session(&token_hash, &state.db).await {
        Ok(Some(row)) => row,
        Ok(None) => return None,
        Err(err) => {
            tracing::error!(error = %err, "session resolution query failed (try_authenticated_user)");
            return None;
        }
    };

    if requires_step_up {
        return None;
    }

    Some(AuthenticatedUser {
        user_id,
        role_keys: role_keys.unwrap_or_default(),
        permission_keys: permission_keys.unwrap_or_default().into_iter().collect(),
        token_hash,
        elevated_until,
        requires_step_up,
        passkey_reverified_until,
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

/// Shared 403 for an authenticated caller whose permissions do not permit
/// the action -- see `AuthenticatedUser::require_permission`, which
/// records an `AUTHORIZATION_FAILURE` audit row and returns this. A
/// single shared response, not a per-handler one, so the body stays
/// consistent across every place this can happen.
pub fn insufficient_role() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ApiErrorBody {
            error: "insufficient_role",
            message: "Your role does not permit this action.".to_string(),
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
///
/// `app.current_user_roles` is comma-joined role keys (e.g.
/// `"admin,onboarding_manager"`) -- see `auth.current_user_has_role` for
/// the matching read side. A held role takes effect on this caller's
/// very next request, since nothing here caches across requests; there is
/// no separate cache to invalidate when an admin changes someone's roles.
pub async fn begin_rls_transaction<'a>(
    pool: &'a PgPool,
    user_id: Uuid,
    role_keys: &[String],
) -> Result<sqlx::Transaction<'a, sqlx::Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;

    sqlx::query("SELECT set_config('app.current_user_id', $1, true)")
        .bind(user_id.to_string())
        .execute(&mut *tx)
        .await?;

    sqlx::query("SELECT set_config('app.current_user_roles', $1, true)")
        .bind(role_keys.join(","))
        .execute(&mut *tx)
        .await?;

    Ok(tx)
}

/// Like `begin_rls_transaction`, but sets ONLY `app.current_user_id` and
/// deliberately leaves `app.current_user_roles` unset.
///
/// For pre-authentication flows that must write a row owned by a
/// specific user before that user has any established role to assert --
/// today, the bootstrap half of passkey registration (see
/// `api::auth_register`), where no session exists yet by definition.
///
/// Strictly LESS privilege than `begin_rls_transaction`, not a
/// convenience variant: every admin-bypass branch in this schema's
/// policies is written as `auth.current_user_has_role('admin')`, which
/// checks membership in `app.current_user_roles` and safely evaluates
/// false when the setting is unset. So a transaction opened here can
/// reach owner-scoped rows and nothing else, and cannot accidentally
/// inherit admin visibility.
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

    fn user_with_elevation(elevated_until: Option<DateTime<Utc>>) -> AuthenticatedUser {
        AuthenticatedUser {
            user_id: Uuid::new_v4(),
            role_keys: vec!["admin".to_string()],
            permission_keys: HashSet::new(),
            token_hash: vec![0u8; 32],
            elevated_until,
            requires_step_up: false,
            passkey_reverified_until: None,
        }
    }

    fn user_with_passkey_reverification(
        passkey_reverified_until: Option<DateTime<Utc>>,
    ) -> AuthenticatedUser {
        AuthenticatedUser {
            passkey_reverified_until,
            ..user_with_elevation(None)
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

    #[test]
    fn has_permission_matches_a_granted_permission_key_only() {
        let mut user = user_with_elevation(None);
        user.permission_keys.insert("users.manage".to_string());
        assert!(user.has_permission("users.manage"));
        assert!(!user.has_permission("client_ops.perform"));
    }

    #[test]
    fn never_reverified_is_not_passkey_reverified() {
        assert!(!user_with_passkey_reverification(None).is_passkey_reverified());
    }

    #[test]
    fn a_future_reverification_deadline_is_passkey_reverified() {
        let deadline = Utc::now() + chrono::Duration::minutes(5);
        assert!(user_with_passkey_reverification(Some(deadline)).is_passkey_reverified());
    }

    #[test]
    fn a_past_reverification_deadline_is_not_passkey_reverified() {
        let deadline = Utc::now() - chrono::Duration::seconds(1);
        assert!(!user_with_passkey_reverification(Some(deadline)).is_passkey_reverified());
    }

    /// The two step-up mechanisms are independent -- proving a TOTP code
    /// must not somehow also count as having proved a passkey, or vice
    /// versa (that would defeat the reason there are two separate columns
    /// at all: each protects changes to the *other* factor).
    #[test]
    fn elevation_and_passkey_reverification_do_not_imply_each_other() {
        let deadline = Utc::now() + chrono::Duration::minutes(5);

        let elevated_only = user_with_elevation(Some(deadline));
        assert!(elevated_only.is_elevated());
        assert!(!elevated_only.is_passkey_reverified());

        let reverified_only = user_with_passkey_reverification(Some(deadline));
        assert!(reverified_only.is_passkey_reverified());
        assert!(!reverified_only.is_elevated());
    }

    /// Integration test for the exact gap that already caused a live
    /// incident: `query_session` queried a `permission_keys` column that
    /// no migration had actually added, caught only when a real login hit
    /// it in production (see migration `resolve_session_returns_permission_keys`'s
    /// own commit message). Nothing in this crate's fast, offline test
    /// suite could have caught that -- every other test here (and
    /// throughout this crate) runs against `test_support::empty_state()`'s
    /// deliberately-unreachable pool, which fails on connection, never on
    /// a bad query.
    ///
    /// Needs a real, reachable Postgres with every migration applied
    /// (`DATABASE_URL` from `.env.local`) -- `#[ignore]`d so the fast
    /// offline suite this crate otherwise is stays fast and offline. Run
    /// explicitly with `cargo test -- --ignored query_session` after any
    /// change to `resolve_session` or to what `query_session` selects
    /// from it -- neither repo has CI yet to run this automatically, so
    /// it depends on a human remembering to.
    ///
    /// A freshly generated token_hash matches no real session, so
    /// `resolve_session`'s `UPDATE ... RETURNING` returns zero rows -- but
    /// Postgres validates every column named in a query's SELECT/RETURNING
    /// list at parse time regardless of whether any row ever matches, so a
    /// query referencing a column that doesn't exist fails exactly the
    /// same way here as it did in production. No session or user fixture
    /// is needed at all.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
    async fn query_sessions_own_sql_is_valid_against_the_real_schema() {
        let _ = dotenvy::from_filename(".env.local");

        let db = crate::db::connect()
            .expect("DATABASE_URL must be a well-formed connection string -- see .env.local");

        let (_, token_hash) = crate::auth::generate_token();

        let result = query_session(&token_hash, &db).await;

        assert!(
            result.is_ok(),
            "query_session's SQL must be valid against the real, migrated schema -- got: {:?}",
            result.err()
        );
        assert_eq!(
            result.unwrap(),
            None,
            "a freshly generated token_hash must match no real session"
        );
    }
}
