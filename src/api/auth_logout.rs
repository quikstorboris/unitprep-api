//! Sign-out and sign-out-everywhere (Phase 2 task 10).
//!
//! ## Why these do not use the `AuthenticatedUser` extractor
//!
//! Deliberately, and it is the main design decision here. Signing out is
//! **idempotent and must never fail**: a stale, revoked, expired or simply
//! unrecognised cookie should still end with the cookie cleared and a
//! success response. Requiring a valid session would answer 401 to someone
//! trying to log out, leaving the dead cookie in place -- the one outcome
//! that guarantees every subsequent request also fails.
//!
//! So these handlers read the cookie directly, ask the database to revoke
//! whatever it matches, and clear the cookie regardless of what came back.
//!
//! ## The response is identical whether anything was revoked
//!
//! `revoked_count` is reported, but the status is always 200 and the cookie
//! is always cleared. A caller learns nothing about whether the token it
//! presented was real, which matters because these endpoints are reachable
//! unauthenticated by design -- otherwise "log out" would double as "is this
//! token valid?".
//!
//! ## Why revocation is a function call rather than an UPDATE
//!
//! `app_service` holds no UPDATE privilege on `auth.sessions` and must not
//! get one: a column grant permits writing NULL as readily as a timestamp,
//! which would hand the application an un-revoke primitive and defeat the
//! reason an opaque session token was chosen over a JWT. See
//! `migrations/20260730140000_revoke_sessions.up.sql` for the full argument.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use axum_extra::extract::cookie::CookieJar;
use serde::Serialize;
use uuid::Uuid;

use crate::api::AppState;
use crate::auth::{audit_log, clear_session_cookie, hash_token, read_session_cookie};

#[derive(Debug, Serialize)]
pub struct LogoutResponse {
    /// Always true. Sign-out is idempotent, so this reports "you are signed
    /// out", not "something was changed".
    pub success: bool,

    /// How many sessions this call revoked. Zero is an ordinary outcome --
    /// an already-revoked or unrecognised cookie -- and is not an error.
    pub revoked_count: i32,
}

/// Which sessions a sign-out reaches. Exists so the two handlers share one
/// body and cannot drift in how they clear cookies or write audit rows.
#[derive(Clone, Copy)]
enum Scope {
    /// Just the session whose token was presented.
    Current,
    /// Every live session belonging to the presenting token's owner.
    Everywhere,
}

impl Scope {
    fn sql(self) -> &'static str {
        match self {
            Scope::Current => "SELECT user_id, revoked_count FROM auth.revoke_session($1)",
            Scope::Everywhere => {
                "SELECT user_id, revoked_count FROM auth.revoke_all_sessions_for_token($1)"
            }
        }
    }

    fn label(self) -> &'static str {
        match self {
            Scope::Current => "current",
            Scope::Everywhere => "everywhere",
        }
    }
}

pub async fn logout(State(state): State<AppState>, jar: CookieJar, headers: HeaderMap) -> Response {
    sign_out(state, jar, headers, Scope::Current).await
}

pub async fn logout_everywhere(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
) -> Response {
    sign_out(state, jar, headers, Scope::Everywhere).await
}

async fn sign_out(state: AppState, jar: CookieJar, headers: HeaderMap, scope: Scope) -> Response {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());

    // Read BEFORE clearing, and the order is load-bearing. Clearing adds an
    // expired cookie with an empty value (see `clear_session_cookie` for why
    // it cannot use `remove`), so reading afterwards yields `Some("")` --
    // which hashes to a token matching nothing. The first draft of this
    // handler had these two lines the other way round, and the effect was a
    // sign-out that cleared the browser's cookie while never revoking the
    // session server-side: the exact failure session_cookie.rs's own doc
    // comment warns about, and invisible from the client because the
    // response is identical either way.
    let presented_token = read_session_cookie(&jar);

    // Cleared on every path below, including the database-error branch, so
    // no branch can forget.
    let jar = clear_session_cookie(jar);

    let Some(raw_token) = presented_token else {
        // No cookie at all. Nothing to revoke and nothing to record: an
        // audit row here would say only that an unauthenticated caller
        // posted to a logout endpoint, which is noise, and it would let
        // anyone fill the trail by looping on it.
        tracing::debug!("sign-out with no session cookie");
        return signed_out(jar, 0);
    };

    let token_hash = hash_token(&raw_token);

    let revoked: Result<(Option<Uuid>, i32), sqlx::Error> = sqlx::query_as(scope.sql())
        .bind(&token_hash)
        .fetch_one(&state.db)
        .await;

    match revoked {
        Ok((Some(user_id), count)) => {
            audit_log::record(
                &state.db,
                audit_log::event::SESSION_REVOKED,
                audit_log::Subjects::by(user_id),
                user_agent,
                serde_json::json!({ "scope": scope.label(), "revoked_count": count }),
            )
            .await;

            tracing::info!(
                user_id = %user_id,
                scope = scope.label(),
                revoked_count = count,
                "signed out"
            );

            signed_out(jar, count)
        }

        // The token matched nothing revocable: already revoked, expired
        // (for the everywhere scope), or never existed. Indistinguishable
        // from success to the caller by design.
        Ok((None, _)) => {
            tracing::debug!(scope = scope.label(), "sign-out matched no live session");
            signed_out(jar, 0)
        }

        // The cookie is cleared regardless. Reporting failure here would
        // leave the caller believing they are still signed in while their
        // browser has already dropped the cookie -- the least useful
        // combination available. The session may genuinely still be live in
        // the database, which is why this is `error` and not `warn`: it
        // needs looking at.
        Err(err) => {
            tracing::error!(
                error = %err,
                scope = scope.label(),
                "failed to revoke session(s) during sign-out"
            );
            signed_out(jar, 0)
        }
    }
}

fn signed_out(jar: CookieJar, revoked_count: i32) -> Response {
    (
        StatusCode::OK,
        jar,
        Json(LogoutResponse {
            success: true,
            revoked_count,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::empty_state;

    /// Builds the jar the way a real request does -- parsed from a `Cookie`
    /// header -- rather than by adding to an empty jar.
    ///
    /// The distinction is not cosmetic. `cookie::CookieJar` treats parsed
    /// cookies as *originals* and added ones as pending changes, and the two
    /// behave differently on removal. A test that constructs the jar by
    /// adding is testing a state no request ever produces.
    fn jar_holding_a_session(token: &str) -> CookieJar {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("{WIRE_COOKIE_NAME}={token}").parse().unwrap(),
        );
        CookieJar::from_headers(&headers)
    }

    /// The cookie name is a LITERAL here rather than the constant, on
    /// purpose. This asserts an on-the-wire contract: browsers already
    /// holding `unitprep_session` can only be cleared by a Set-Cookie using
    /// that exact name, so renaming the constant is a breaking change for
    /// every signed-in client and should fail a test rather than silently
    /// compile. Importing the constant would make the assertion tautological.
    const WIRE_COOKIE_NAME: &str = "unitprep_session";

    fn cleared(response: &Response) -> bool {
        response
            .headers()
            .get_all(axum::http::header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|value| value.starts_with(WIRE_COOKIE_NAME) && value.contains("Path=/"))
    }

    /// No cookie is the commonest real case -- a user clicking sign out on a
    /// page whose session already expired. It must succeed without touching
    /// the database. `empty_state`'s pool is unreachable, so a query would
    /// surface as a delay-then-error rather than a clean 200.
    #[tokio::test]
    async fn signing_out_without_a_cookie_succeeds_and_clears() {
        let response = logout(State(empty_state()), CookieJar::new(), HeaderMap::new()).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            cleared(&response),
            "the session cookie must be cleared even when none was presented"
        );
    }

    /// The database is unreachable in tests, so this exercises the error
    /// branch: sign-out must still answer 200 with the cookie cleared. A
    /// failure here would mean a database blip leaves users unable to log
    /// out while their browser has already dropped the cookie.
    #[tokio::test]
    async fn a_database_failure_still_clears_the_cookie_and_reports_success() {
        let jar = jar_holding_a_session("some-token");

        let response = logout(State(empty_state()), jar, HeaderMap::new()).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(cleared(&response));
    }

    /// Same guarantees for the everywhere scope -- written separately rather
    /// than assumed, because it is a different SQL path and the shared body
    /// is exactly the kind of thing a later refactor splits.
    #[tokio::test]
    async fn signing_out_everywhere_has_the_same_guarantees() {
        let jar = jar_holding_a_session("some-token");

        let response = logout_everywhere(State(empty_state()), jar, HeaderMap::new()).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(cleared(&response));
    }
}
