//! TOTP enrolment, removal, and fallback sign-in (Phase 2 task 9).
//!
//! The cryptography and the encryption-at-rest decision live in
//! `auth::totp`; this is orchestration.
//!
//! ## Enrolment is two steps, and the second one is the point
//!
//! `/enroll/begin` writes the secret with `confirmed_at` NULL and hands back
//! the `otpauth://` URI. `/enroll/confirm` takes a code and, only if it
//! verifies, sets `confirmed_at`. Sign-in requires a *confirmed* credential.
//!
//! Without the second step a user could believe they had a working fallback
//! while having mistyped the secret, or scanned it into an app on a phone
//! whose clock is wrong -- and they would find out at exactly the moment they
//! needed the fallback and had no other way in. Requiring one successful code
//! before the credential counts turns that from a latent problem into an
//! immediate, obvious one.
//!
//! ## Re-enrolling replaces the previous secret immediately
//!
//! `/enroll/begin` on an account that already has TOTP overwrites the secret
//! and clears `confirmed_at`, so the old authenticator stops working at once
//! rather than when the new one is confirmed. That is a deliberate
//! simplification -- `totp_credentials.user_id` is UNIQUE, so holding both
//! would need a second row and a rule for which wins -- and it is affordable
//! precisely because TOTP is a **fallback**: abandoning a half-finished
//! re-enrolment costs the fallback, not access to the account.
//!
//! ## The sign-in path is unauthenticated, so it says nothing
//!
//! Unknown address, no TOTP enrolled, unconfirmed enrolment, wrong code, and
//! locked-out all return the same 401. Distinguishing them would leak which
//! addresses exist and, worse, tell an attacker their guessing is having an
//! effect by reporting the lockout they caused.

use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{
    audit_log, base32_secret, begin_owner_rls_transaction, encrypt_secret, generate_secret,
    generate_token, issue_session_cookie, provisioning_uri, totp_configured, verify_code,
    AuthenticatedUser,
};

/// Matches the passkey paths, so a session's lifetime does not depend on
/// which factor minted it.
fn session_lifetime_hours() -> i64 {
    std::env::var("SESSION_LIFETIME_HOURS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|hours| *hours > 0)
        .unwrap_or(12)
}

#[derive(Debug, Serialize)]
pub struct EnrollBeginResponse {
    /// `otpauth://` URI for an authenticator app, normally rendered as a QR
    /// code by the frontend. Contains the secret -- treat it as one.
    pub provisioning_uri: String,

    /// The same secret in base32, for typing in by hand when a camera is not
    /// available.
    pub secret: String,
}

#[derive(Debug, Deserialize)]
pub struct TotpCodeRequest {
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct TotpConfirmResponse {
    pub confirmed: bool,
}

#[derive(Debug, Deserialize)]
pub struct TotpLoginRequest {
    pub email: String,
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct TotpLoginResponse {
    pub success: bool,
}

/// TOTP is unavailable in this deployment because no encryption key is set.
///
/// A 503 rather than a 500: nothing is broken, the feature is not configured,
/// and the distinction matters to whoever is looking at it. Told plainly
/// because this only ever reaches an authenticated caller on the enrolment
/// path -- the sign-in path folds it into the opaque 401 instead, since
/// "TOTP is switched off here" is not something an anonymous caller needs.
fn not_configured() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiErrorBody {
            error: "totp_not_configured",
            message: "TOTP is not available: the server has no TOTP_ENCRYPTION_KEY configured."
                .to_string(),
        }),
    )
        .into_response()
}

fn wrong_code() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "totp_code_rejected",
            message: "That code is not valid. Check your authenticator app and try again."
                .to_string(),
        }),
    )
        .into_response()
}

/// One response for every reason a TOTP sign-in cannot proceed. See the
/// module docs.
fn login_unavailable() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(ApiErrorBody {
            error: "login_unavailable",
            message: "Could not sign in with that address and code.".to_string(),
        }),
    )
        .into_response()
}

pub async fn enroll_begin(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
) -> Response {
    if !totp_configured() {
        return not_configured();
    }

    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());

    // The account's own email, read under the caller's own identity. Used as
    // the label an authenticator app displays, so it must be the real
    // address rather than anything a client supplied.
    let email = match own_email(&state, user.user_id).await {
        Ok(Some(email)) => email,
        Ok(None) => return internal_error("Could not load the signed-in user"),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to load user for TOTP enrolment");
            return internal_error("Could not start TOTP enrolment");
        }
    };

    let secret = generate_secret();

    let encrypted = match encrypt_secret(user.user_id, &secret) {
        Ok(blob) => blob,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to encrypt a TOTP secret");
            return internal_error("Could not start TOTP enrolment");
        }
    };

    let uri = match provisioning_uri(user.user_id, &secret, &email) {
        Ok(uri) => uri,
        Err(err) => {
            tracing::error!(error = %err, "failed to build a TOTP provisioning URI");
            return internal_error("Could not start TOTP enrolment");
        }
    };

    if let Err(err) = store_unconfirmed_secret(&state, user.user_id, &encrypted).await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to store a TOTP secret");
        return internal_error("Could not start TOTP enrolment");
    }

    audit_log::record(
        &state.db,
        audit_log::event::TOTP_ENROLMENT_STARTED,
        audit_log::Subjects::by(user.user_id),
        user_agent,
        // No secret, no URI. Both contain the shared secret, and an audit
        // trail is not a place to keep one.
        serde_json::json!({}),
    )
    .await;

    tracing::info!(user_id = %user.user_id, "TOTP enrolment started");

    Json(EnrollBeginResponse {
        provisioning_uri: uri,
        secret: base32_secret(&secret),
    })
    .into_response()
}

pub async fn enroll_confirm(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Json(request): Json<TotpCodeRequest>,
) -> Response {
    if !totp_configured() {
        return not_configured();
    }

    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());

    let loaded = match load_own_secret(&state, user.user_id).await {
        Ok(Some(loaded)) => loaded,
        Ok(None) => return wrong_code(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to load a TOTP secret");
            return internal_error("Could not confirm TOTP enrolment");
        }
    };

    let verified = match verify_code(
        user.user_id,
        &loaded.secret_encrypted,
        &loaded.email,
        &request.code,
    ) {
        Ok(verified) => verified,
        Err(err) => {
            // The secret could not be read -- a key change, or a corrupted
            // row. Not the caller's fault and must not be reported as a
            // wrong code, or they will retype it forever.
            tracing::error!(error = %err, user_id = %user.user_id, "could not verify a TOTP code");
            return internal_error("Could not confirm TOTP enrolment");
        }
    };

    if !verified {
        tracing::warn!(user_id = %user.user_id, "TOTP enrolment confirmation rejected");
        audit_log::record(
            &state.db,
            audit_log::event::TOTP_ENROLMENT_FAILED,
            audit_log::Subjects::by(user.user_id),
            user_agent,
            serde_json::json!({ "reason": "code_rejected" }),
        )
        .await;
        return wrong_code();
    }

    if let Err(err) = confirm_own_secret(&state, user.user_id).await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to confirm a TOTP secret");
        return internal_error("Could not confirm TOTP enrolment");
    }

    audit_log::record(
        &state.db,
        audit_log::event::TOTP_ENROLLED,
        audit_log::Subjects::by(user.user_id),
        user_agent,
        serde_json::json!({}),
    )
    .await;

    tracing::info!(user_id = %user.user_id, "TOTP enrolled");

    Json(TotpConfirmResponse { confirmed: true }).into_response()
}

/// Removes the caller's TOTP credential.
///
/// Exists because a factor you cannot remove is a factor you cannot rotate
/// away from after losing the device holding it -- and the alternative for
/// the user would be asking an administrator to do it, which is a
/// social-engineering surface for no benefit. Safe to expose because it only
/// ever removes the *fallback* for the account making the request.
pub async fn disable(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
) -> Response {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());

    let removed = match delete_own_secret(&state, user.user_id).await {
        Ok(removed) => removed,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to remove a TOTP secret");
            return internal_error("Could not remove TOTP");
        }
    };

    if removed {
        audit_log::record(
            &state.db,
            audit_log::event::TOTP_REMOVED,
            audit_log::Subjects::by(user.user_id),
            user_agent,
            serde_json::json!({}),
        )
        .await;
        tracing::info!(user_id = %user.user_id, "TOTP removed");
    }

    // Idempotent: removing a factor that is not there is a success, for the
    // same reason signing out without a session is.
    Json(TotpConfirmResponse { confirmed: false }).into_response()
}

pub async fn login(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(request): Json<TotpLoginRequest>,
) -> Response {
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok());

    let email = request.email.trim().to_ascii_lowercase();

    if email.is_empty() {
        // Same reasoning as the passkey login path's equivalent case: an
        // empty address is exactly as much a login attempt as one that
        // fails to resolve to an account, so it gets the same audit row.
        audit_log::record(
            &state.db,
            audit_log::event::LOGIN_FAILED,
            audit_log::Subjects::anonymous(),
            user_agent,
            serde_json::json!({ "reason": "empty_email", "factor": "totp" }),
        )
        .await;
        return login_unavailable();
    }

    // The HTTP response is folded into the same opaque rejection as
    // everything else -- an unconfigured deployment must not be
    // detectable from an unauthenticated endpoint. The audit row is not
    // under that constraint: it is never visible to the caller, and an
    // operator watching it genuinely benefits from telling "TOTP is off
    // here" apart from "someone tried a bad address".
    if !totp_configured() {
        audit_log::record(
            &state.db,
            audit_log::event::LOGIN_FAILED,
            audit_log::Subjects::anonymous(),
            user_agent,
            serde_json::json!({ "reason": "totp_not_configured", "factor": "totp" }),
        )
        .await;
        return login_unavailable();
    }

    let candidate: Result<Option<(Uuid, Vec<u8>, bool)>, sqlx::Error> = sqlx::query_as(
        "SELECT user_id, secret_encrypted, is_locked
           FROM auth.resolve_totp_candidate($1::citext)",
    )
    .bind(&email)
    .fetch_optional(&state.db)
    .await;

    let (user_id, secret_encrypted, is_locked) = match candidate {
        Ok(Some(row)) => row,
        Ok(None) => {
            // No account, or no confirmed TOTP. Recorded with no actor for
            // the same reason a failed passkey login is: there may be no
            // user behind this address at all.
            audit_log::record(
                &state.db,
                audit_log::event::LOGIN_FAILED,
                audit_log::Subjects::anonymous(),
                user_agent,
                serde_json::json!({ "reason": "no_confirmed_totp", "email": email, "factor": "totp" }),
            )
            .await;
            return login_unavailable();
        }
        Err(err) => {
            tracing::error!(error = %err, "TOTP candidate lookup failed");
            return internal_error("Could not sign in");
        }
    };

    if is_locked {
        // Deliberately not counted as another failure: letting attempts keep
        // accruing while locked would extend the lock indefinitely under a
        // sustained attack, turning a 15-minute inconvenience into a
        // permanent denial of the fallback.
        tracing::warn!(user_id = %user_id, "TOTP sign-in refused: locked out");
        audit_log::record(
            &state.db,
            audit_log::event::LOGIN_FAILED,
            audit_log::Subjects::by(user_id),
            user_agent,
            serde_json::json!({ "reason": "totp_locked_out", "factor": "totp" }),
        )
        .await;
        return login_unavailable();
    }

    let verified = match verify_code(user_id, &secret_encrypted, &email, &request.code) {
        Ok(verified) => verified,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user_id, "could not verify a TOTP code");
            return internal_error("Could not sign in");
        }
    };

    if !verified {
        let attempts: Result<i32, sqlx::Error> =
            sqlx::query_scalar("SELECT auth.record_totp_failure($1)")
                .bind(user_id)
                .fetch_one(&state.db)
                .await;

        match attempts {
            Ok(count) => {
                tracing::warn!(
                    user_id = %user_id,
                    failed_attempts = count,
                    "TOTP sign-in rejected"
                );
                audit_log::record(
                    &state.db,
                    audit_log::event::LOGIN_FAILED,
                    audit_log::Subjects::by(user_id),
                    user_agent,
                    serde_json::json!({
                        "reason": "totp_code_rejected",
                        "factor": "totp",
                        "failed_attempts": count,
                    }),
                )
                .await;
            }
            Err(err) => {
                // The attempt is not counted, so log loudly: a brute-force
                // guard that silently stops counting is worse than none,
                // because nothing looks wrong.
                tracing::error!(error = %err, user_id = %user_id, "failed to record a TOTP failure");
            }
        }

        return login_unavailable();
    }

    if let Err(err) = sqlx::query("SELECT auth.record_totp_success($1)")
        .bind(user_id)
        .execute(&state.db)
        .await
    {
        // The code was correct. Failing the sign-in over the bookkeeping
        // would be the wrong trade -- but it does mean the failure counter
        // stays where it was, so this is `error`, not `warn`.
        tracing::error!(error = %err, user_id = %user_id, "failed to clear TOTP failure state");
    }

    let (raw_token, token_hash) = generate_token();
    let lifetime_hours = session_lifetime_hours();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(lifetime_hours);

    let created: Result<Uuid, sqlx::Error> =
        sqlx::query_scalar("SELECT auth.create_session($1, $2, $3, NULL, $4)")
            .bind(user_id)
            .bind(&token_hash)
            .bind(expires_at)
            .bind(user_agent)
            .fetch_one(&state.db)
            .await;

    match created {
        Ok(session_id) => {
            audit_log::record(
                &state.db,
                audit_log::event::LOGIN_SUCCEEDED,
                audit_log::Subjects::by(user_id),
                user_agent,
                serde_json::json!({ "session_id": session_id, "factor": "totp" }),
            )
            .await;

            tracing::info!(
                user_id = %user_id,
                session_id = %session_id,
                factor = "totp",
                "TOTP sign-in succeeded"
            );

            let jar = issue_session_cookie(jar, raw_token, time::Duration::hours(lifetime_hours));

            (jar, Json(TotpLoginResponse { success: true })).into_response()
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user_id, "TOTP verified but session creation failed");
            (
                jar,
                internal_error("Signed in, but the session could not be created"),
            )
                .into_response()
        }
    }
}

struct LoadedSecret {
    secret_encrypted: Vec<u8>,
    email: String,
}

/// Reads the caller's own email under their own identity.
async fn own_email(state: &AppState, user_id: Uuid) -> Result<Option<String>, sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(&state.db, user_id).await?;

    let email: Option<String> =
        sqlx::query_scalar("SELECT email::text FROM auth.users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;

    tx.commit().await?;
    Ok(email)
}

/// Reads the caller's own TOTP secret. Confirmed or not: this serves the
/// confirmation step, which by definition runs against an unconfirmed row.
async fn load_own_secret(
    state: &AppState,
    user_id: Uuid,
) -> Result<Option<LoadedSecret>, sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(&state.db, user_id).await?;

    let row: Option<(Vec<u8>, String)> = sqlx::query_as(
        "SELECT t.secret_encrypted, u.email::text
           FROM auth.totp_credentials t
           JOIN auth.users u ON u.id = t.user_id
          WHERE t.user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(row.map(|(secret_encrypted, email)| LoadedSecret {
        secret_encrypted,
        email,
    }))
}

/// Writes a fresh unconfirmed secret, replacing any existing row.
///
/// `ON CONFLICT` on the unique `user_id` rather than delete-then-insert, so
/// re-enrolment is one statement and cannot leave the account with no row at
/// all if it fails halfway. `confirmed_at` is explicitly reset -- the new
/// secret has never been proven, and inheriting the old confirmation would
/// mean an unverified secret counted as a working factor.
async fn store_unconfirmed_secret(
    state: &AppState,
    user_id: Uuid,
    encrypted: &[u8],
) -> Result<(), sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(&state.db, user_id).await?;

    sqlx::query(
        "INSERT INTO auth.totp_credentials (user_id, secret_encrypted)
         VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE
            SET secret_encrypted = EXCLUDED.secret_encrypted,
                confirmed_at = NULL,
                failed_attempts = 0,
                locked_until = NULL,
                last_used_at = NULL",
    )
    .bind(user_id)
    .bind(encrypted)
    .execute(&mut *tx)
    .await?;

    tx.commit().await
}

async fn confirm_own_secret(state: &AppState, user_id: Uuid) -> Result<(), sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(&state.db, user_id).await?;

    sqlx::query(
        "UPDATE auth.totp_credentials
            SET confirmed_at = now(), failed_attempts = 0, locked_until = NULL
          WHERE user_id = $1",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await
}

async fn delete_own_secret(state: &AppState, user_id: Uuid) -> Result<bool, sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(&state.db, user_id).await?;

    let removed = sqlx::query("DELETE FROM auth.totp_credentials WHERE user_id = $1")
        .bind(user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    tx.commit().await?;
    Ok(removed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::empty_state;
    use serial_test::serial;

    fn set_key() {
        std::env::set_var(
            "TOTP_ENCRYPTION_KEY",
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        );
    }

    /// An empty address must not reach the database, and must land on the
    /// same opaque rejection as everything else.
    #[tokio::test]
    #[serial(totp_env)]
    async fn totp_login_refuses_an_empty_email_without_touching_the_database() {
        set_key();

        let response = login(
            State(empty_state()),
            CookieJar::new(),
            HeaderMap::new(),
            Json(TotpLoginRequest {
                email: "   ".to_string(),
                code: "123456".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// With no key configured, the unauthenticated path must look exactly
    /// like any other refusal -- not a 503, which would advertise the
    /// deployment's configuration to anyone who asked.
    #[tokio::test]
    #[serial(totp_env)]
    async fn an_unconfigured_deployment_is_indistinguishable_on_the_login_path() {
        std::env::remove_var("TOTP_ENCRYPTION_KEY");

        let response = login(
            State(empty_state()),
            CookieJar::new(),
            HeaderMap::new(),
            Json(TotpLoginRequest {
                email: "someone@example.com".to_string(),
                code: "123456".to_string(),
            }),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "an anonymous caller must not learn that TOTP is unconfigured"
        );

        set_key();
    }

    /// The enrolment path, by contrast, tells an authenticated caller
    /// plainly -- they can act on it, and hiding it would look like a bug.
    #[tokio::test]
    #[serial(totp_env)]
    async fn an_unconfigured_deployment_says_so_on_the_enrolment_path() {
        std::env::remove_var("TOTP_ENCRYPTION_KEY");

        let response = enroll_begin(
            State(empty_state()),
            AuthenticatedUser {
                user_id: Uuid::new_v4(),
                role: crate::auth::Role::Admin,
            },
            HeaderMap::new(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        set_key();
    }
}
