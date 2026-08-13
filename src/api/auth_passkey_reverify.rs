//! Passkey-based re-verification for an already-signed-in caller -- the
//! mirror of TOTP step-up (`auth_totp::step_up`), but proving the
//! *other* factor. Gates self-service TOTP re-enrolment: TOTP already
//! step-up-gates replacing a passkey (`add_passkey` in
//! `step_up_actions`); this is what gates replacing TOTP, so a hijacked
//! session cannot silently swap in an attacker-controlled TOTP secret
//! and then use it to pass the existing `add_passkey` step-up check.
//!
//! Reuses `AuthenticationCeremony`/`authentication_ceremonies` wholesale
//! -- structurally this is exactly a login ceremony's two halves
//! (challenge, then verify), just scoped to an already-known `user_id`
//! (the caller's own, from `AuthenticatedUser`) instead of one resolved
//! from an anonymous email lookup. Like TOTP's own `step_up`, this never
//! issues a session or touches the session cookie -- the caller is
//! already signed in, or this handler would not have been reached at
//! all.

use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};
use unitprep_core::session_store::SessionStoreExt;
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{
    audit_log, clear_ceremony_cookie, issue_ceremony_cookie, read_ceremony_cookie,
    AuthenticatedUser, AuthenticationCeremony, REVERIFY_CEREMONY_COOKIE,
};

use super::auth_login::{load_credentials_for_user, persist_credential_use};

/// How long a successful reverification elevates the caller's session
/// for -- same window TOTP step-up uses (see auth_totp::STEP_UP_MINUTES),
/// for the same reason: long enough to finish the re-enrolment flow it
/// gates without re-proving mid-task, short enough that a walked-away
/// session can't ride on one earlier assertion indefinitely.
const REVERIFY_MINUTES: i32 = 5;

/// Same ceremony lifetime the login/registration ceremonies use -- one
/// request/response round trip through the browser's own
/// `navigator.credentials.get()` call.
const CEREMONY_TTL_MINUTES: i64 = 5;

#[derive(Debug, Serialize)]
pub struct ReverifyBeginResponse {
    pub challenge: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct ReverifyFinishRequest {
    pub credential: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ReverifyFinishResponse {
    pub verified: bool,
}

fn ceremony_not_found() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "ceremony_not_found",
            message: "This verification attempt has expired or was never started. Try again."
                .to_string(),
        }),
    )
        .into_response()
}

fn ceremony_failed() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ApiErrorBody {
            error: "reverify_failed",
            message: "That passkey could not be verified.".to_string(),
        }),
    )
        .into_response()
}

fn no_passkey_registered() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "no_passkey_registered",
            message: "No passkey is registered on this account to verify against.".to_string(),
        }),
    )
        .into_response()
}

fn unauthorized_session_gone() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ApiErrorBody {
            error: "unauthorized",
            message: "Your session ended. Sign in again.".to_string(),
        }),
    )
        .into_response()
}

pub async fn reverify_begin(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    jar: CookieJar,
) -> Response {
    let stored = match load_credentials_for_user(&state, user.user_id).await {
        Ok(stored) => stored,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to load credentials for passkey reverify");
            return internal_error("Could not start passkey verification");
        }
    };

    if stored.is_empty() {
        return no_passkey_registered();
    }

    let challenge = match state.auth_backend.start_authentication(&stored) {
        Ok(challenge) => challenge,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to start a passkey reverify ceremony");
            return internal_error("Could not start passkey verification");
        }
    };

    let ceremony_id = Uuid::new_v4().to_string();

    let ceremony = AuthenticationCeremony::new(ceremony_id.clone(), user.user_id, challenge.state);

    // Read before the store takes ownership -- same reasoning as
    // login_begin's identical line.
    let correlation_id = ceremony.correlation_id;

    state.authentication_ceremonies.save(ceremony);

    let jar = issue_ceremony_cookie(
        jar,
        REVERIFY_CEREMONY_COOKIE,
        ceremony_id,
        time::Duration::minutes(CEREMONY_TTL_MINUTES),
    );

    tracing::info!(
        user_id = %user.user_id,
        correlation_id = %correlation_id,
        "passkey reverify ceremony started"
    );

    (
        jar,
        Json(ReverifyBeginResponse {
            challenge: challenge.challenge,
        }),
    )
        .into_response()
}

pub async fn reverify_finish(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    jar: CookieJar,
    Json(request): Json<ReverifyFinishRequest>,
) -> Response {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());

    let Some(ceremony_id) = read_ceremony_cookie(&jar, REVERIFY_CEREMONY_COOKIE) else {
        return ceremony_not_found();
    };

    // Read and release before any `.await` -- holding a session lock
    // across an await point is forbidden by `SessionStore`'s documented
    // locking invariants.
    let Some((ceremony_user_id, correlation_id, webauthn_state)) = state
        .authentication_ceremonies
        .with_session(&ceremony_id, |ceremony| {
            (
                ceremony.user_id,
                ceremony.correlation_id,
                ceremony.webauthn_state.clone(),
            )
        })
    else {
        return (
            clear_ceremony_cookie(jar, REVERIFY_CEREMONY_COOKIE),
            ceremony_not_found(),
        )
            .into_response();
    };

    // Single-use, consumed BEFORE verification -- same reasoning as
    // login_finish: an attacker replaying assertions against a live
    // challenge is a real attack, not just an untidy retry.
    state.authentication_ceremonies.delete(&ceremony_id);

    let jar = clear_ceremony_cookie(jar, REVERIFY_CEREMONY_COOKIE);

    // A ceremony begun by one signed-in caller must not be completable
    // by a different one -- the ceremony cookie is scoped to whichever
    // browser holds it, but the *caller* finishing it now must still be
    // the same user who started it (e.g. a different account signed in
    // on the same browser mid-ceremony).
    if ceremony_user_id != user.user_id {
        tracing::warn!(
            ceremony_user_id = %ceremony_user_id,
            caller_user_id = %user.user_id,
            correlation_id = %correlation_id,
            "passkey reverify finish called by a different user than started it"
        );
        return (jar, ceremony_not_found()).into_response();
    }

    // Re-read the credential set under the caller's own identity rather
    // than trusting a copy taken at `/begin` -- same reasoning as
    // AuthenticationCeremony's own doc comment on why it doesn't carry
    // the credential set itself.
    let stored = match load_credentials_for_user(&state, user.user_id).await {
        Ok(stored) if !stored.is_empty() => stored,
        Ok(_) => {
            // Every passkey was removed mid-ceremony.
            audit_log::record(
                &state.db,
                audit_log::event::PASSKEY_REVERIFY_FAILED,
                audit_log::Subjects::by(user.user_id),
                user_agent,
                None,
                audit_log::Change::none(),
                serde_json::json!({
                    "reason": "credentials_removed_mid_ceremony",
                    "correlation_id": correlation_id,
                }),
            )
            .await;

            return (jar, ceremony_failed()).into_response();
        }
        Err(err) => {
            tracing::error!(
                error = %err,
                user_id = %user.user_id,
                correlation_id = %correlation_id,
                "failed to reload credentials for passkey reverify"
            );
            return (
                jar,
                internal_error("Could not complete passkey verification"),
            )
                .into_response();
        }
    };

    let outcome =
        match state
            .auth_backend
            .finish_authentication(request.credential, &webauthn_state, &stored)
        {
            Ok(outcome) => outcome,
            Err(err) => {
                // `warn`, not `error`: a failed assertion is an ordinary
                // client-side outcome, not a server fault.
                tracing::warn!(
                    user_id = %user.user_id,
                    correlation_id = %correlation_id,
                    error = %err,
                    "passkey reverify ceremony failed verification"
                );

                audit_log::record(
                    &state.db,
                    audit_log::event::PASSKEY_REVERIFY_FAILED,
                    audit_log::Subjects::by(user.user_id),
                    user_agent,
                    None,
                    audit_log::Change::none(),
                    serde_json::json!({
                        "reason": "assertion_rejected",
                        "correlation_id": correlation_id,
                    }),
                )
                .await;

                return (jar, ceremony_failed()).into_response();
            }
        };

    // The signature counter webauthn-rs just advanced is the
    // anti-cloning mechanism -- not persisting it would leave the check
    // running against a frozen value, same reasoning as login_finish.
    if let Err(err) = persist_credential_use(&state, user.user_id, &outcome).await {
        tracing::error!(
            error = %err,
            user_id = %user.user_id,
            correlation_id = %correlation_id,
            "failed to persist post-reverification credential state"
        );
        return (
            jar,
            internal_error("Could not complete passkey verification"),
        )
            .into_response();
    }

    let elevated: Result<bool, sqlx::Error> =
        sqlx::query_scalar("SELECT auth.record_passkey_reverify($1, $2)")
            .bind(&user.token_hash)
            .bind(REVERIFY_MINUTES)
            .fetch_one(&state.db)
            .await;

    match elevated {
        Ok(true) => {
            audit_log::record(
                &state.db,
                audit_log::event::PASSKEY_REVERIFY_SUCCEEDED,
                audit_log::Subjects::by(user.user_id),
                user_agent,
                None,
                audit_log::Change::none(),
                serde_json::json!({ "correlation_id": correlation_id }),
            )
            .await;

            tracing::info!(user_id = %user.user_id, "passkey reverify succeeded");

            (jar, Json(ReverifyFinishResponse { verified: true })).into_response()
        }
        // The assertion verified, but the session itself vanished
        // (expired or revoked from another device) in the time it took
        // to verify -- same rare race as TOTP step-up's own handling.
        Ok(false) => {
            tracing::warn!(user_id = %user.user_id, "passkey reverify verified but the session was gone");
            (jar, unauthorized_session_gone()).into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to record a passkey reverify");
            (
                jar,
                internal_error("Could not complete passkey verification"),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
#[path = "auth_passkey_reverify_tests.rs"]
mod tests;
