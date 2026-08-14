//! TOTP enrolment and step-up verification for sensitive in-session
//! actions.
//!
//! The cryptography and the encryption-at-rest decision live in
//! `auth::totp`; this is orchestration.
//!
//! ## Not a way to log in
//!
//! TOTP shipped as a login-fallback factor (Phase 2 task 9, 2026-07-30),
//! for a device with no passkey enrolled. That gap is now covered by
//! admin-driven account recovery instead (deactivate -> reissue invite ->
//! re-enrol), so a static shared secret standing in as an equally-capable
//! full login path -- next to a hardware-bound passkey -- was undercutting
//! the account's real security floor rather than helping it. There is no
//! `/auth/login/totp` any more; `/auth/totp/step-up` is authenticated-only,
//! and all it does is extend `auth.sessions.elevated_until` on the
//! caller's own session (see `AuthenticatedUser::is_elevated` in
//! `auth::authenticated_user`) -- it never issues a session or signs
//! anyone in.
//!
//! ## Enrolment is two steps, and the second one is the point
//!
//! `/enroll/begin` writes the secret with `confirmed_at` NULL and hands back
//! the `otpauth://` URI. `/enroll/confirm` takes a code and, only if it
//! verifies, sets `confirmed_at`. Step-up requires a *confirmed* credential.
//!
//! Without the second step a user could believe they had a working step-up
//! factor while having mistyped the secret, or scanned it into an app on a
//! phone whose clock is wrong -- and they would find out at exactly the
//! moment a sensitive action needed it. Requiring one successful code before
//! the credential counts turns that from a latent problem into an immediate,
//! obvious one.
//!
//! ## Re-enrolling keeps the old secret live until the new one is proven
//!
//! `/enroll/begin` writes the new candidate into
//! `totp_credentials.pending_secret_encrypted`, not `secret_encrypted` --
//! the existing confirmed secret (if any) is completely untouched until
//! `/enroll/confirm` verifies a code against the *pending* one, at which
//! point it's promoted into `secret_encrypted` in the same statement that
//! clears `pending_secret_encrypted`. This used to overwrite
//! `secret_encrypted` immediately at `/enroll/begin`, so an abandoned
//! re-enrolment (closed tab, dead battery, anything) left the account with
//! no working step-up factor at all until it was finished -- not a large
//! window, but a real and entirely avoidable one, closed by holding the
//! candidate separately instead of shortening the gap.
//!
//! There is no "disable TOTP" action any more (removed 2026-08-04): TOTP is
//! step-up-only, never a login factor, so there was no security benefit to
//! letting an account have zero step-up factor -- only a self-inflicted
//! friction risk (locking yourself out of step-up-gated actions like
//! adding a passkey until you re-enrol). Re-enrolling replaces a factor;
//! it never removes one with nothing to replace it.

use axum::{
    extract::{Json, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{
    audit_log, base32_secret, begin_owner_rls_transaction, encrypt_secret, generate_secret,
    provisioning_uri, totp_configured, verify_code, AuthenticatedUser,
};

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

#[derive(Debug, Serialize)]
pub struct StepUpResponse {
    pub confirmed: bool,
}

/// How long a step-up verification elevates the caller's session for.
/// Long enough to complete a multi-field sensitive action (entering API
/// credentials, say) without re-verifying mid-task; short enough that a
/// walked-away session can't be abused on the strength of one earlier code.
const STEP_UP_MINUTES: i32 = 5;

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

/// The caller is signed in, but has no confirmed TOTP credential to step up
/// with. Named plainly rather than folded into `wrong_code` -- unlike the
/// old anonymous login path, there is no enumeration concern here: the
/// caller is already authenticated as themselves, so telling them their own
/// account's TOTP state leaks nothing they don't already know.
fn not_enrolled() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "totp_not_enrolled",
            message: "Set up an authenticator app before confirming this action.".to_string(),
        }),
    )
        .into_response()
}

/// Same reasoning as `not_enrolled`: the caller is already authenticated as
/// themselves, so naming the lockout plainly leaks nothing new -- unlike
/// the old anonymous login path, where this had to fold into an opaque
/// rejection.
fn locked_out() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(ApiErrorBody {
            error: "totp_locked_out",
            message: "Too many incorrect codes. Try again in a few minutes.".to_string(),
        }),
    )
        .into_response()
}

/// Self-service TOTP *re*-enrolment (an account that already has a
/// confirmed factor) requires a fresh passkey re-verification first --
/// see auth_passkey_reverify.rs's module docs for why. First-time
/// enrolment (no confirmed factor yet) is never gated by this: there is
/// nothing yet for a hijacked session to be replacing, and first-time
/// setup happens at admin-driven onboarding, not self-service.
fn passkey_reverification_required() -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(ApiErrorBody {
            error: "passkey_reverification_required",
            message: "Verify your passkey before replacing your authenticator app.".to_string(),
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

    match load_own_confirmed_secret(&state, user.user_id).await {
        Ok(Some(_)) if !user.is_passkey_reverified() => {
            return passkey_reverification_required();
        }
        Ok(_) => {}
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to check for an existing confirmed TOTP secret");
            return internal_error("Could not start TOTP enrolment");
        }
    }

    let user_agent = crate::api::user_agent_from(&headers);

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
        None,
        audit_log::Change::none(),
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

    let user_agent = crate::api::user_agent_from(&headers);

    let loaded = match load_own_secret(&state, user.user_id).await {
        Ok(Some(loaded)) => loaded,
        Ok(None) => return wrong_code(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to load a TOTP secret");
            return internal_error("Could not confirm TOTP enrolment");
        }
    };

    // Same lockout enforced here as on step_up -- previously this branch
    // read the pending secret's lock state but never checked it, so an
    // authenticated caller (or a session hijacked before step-up) could
    // submit unlimited guesses against a just-generated candidate secret
    // during enrolment with no rate limit at all.
    if loaded.is_locked {
        tracing::warn!(user_id = %user.user_id, "TOTP enrolment confirmation refused: locked out");
        return locked_out();
    }

    // No prior accepted step for a secret that has never been confirmed --
    // there is nothing yet for this code to replay against.
    let matched_step = match verify_code(
        user.user_id,
        &loaded.secret_encrypted,
        &loaded.email,
        &request.code,
        None,
    ) {
        Ok(matched_step) => matched_step,
        Err(err) => {
            // The secret could not be read -- a key change, or a corrupted
            // row. Not the caller's fault and must not be reported as a
            // wrong code, or they will retype it forever.
            tracing::error!(error = %err, user_id = %user.user_id, "could not verify a TOTP code");
            return internal_error("Could not confirm TOTP enrolment");
        }
    };

    let Some(matched_step) = matched_step else {
        // Same counter step_up's wrong-code branch uses -- shared per
        // user_id, not per pending-vs-confirmed state (confirm_own_secret
        // already resets it on success either way).
        let attempts: Result<i32, sqlx::Error> =
            sqlx::query_scalar("SELECT auth.record_totp_failure($1)")
                .bind(user.user_id)
                .fetch_one(&state.db)
                .await;

        let failed_attempts = match attempts {
            Ok(count) => Some(count),
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "failed to record a TOTP failure during enrolment");
                None
            }
        };

        tracing::warn!(user_id = %user.user_id, "TOTP enrolment confirmation rejected");
        audit_log::record(
            &state.db,
            audit_log::event::TOTP_ENROLMENT_FAILED,
            audit_log::Subjects::by(user.user_id),
            user_agent,
            None,
            audit_log::Change::none(),
            serde_json::json!({ "reason": "code_rejected", "failed_attempts": failed_attempts }),
        )
        .await;
        return wrong_code();
    };

    if let Err(err) = confirm_own_secret(&state, user.user_id, matched_step).await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to confirm a TOTP secret");
        return internal_error("Could not confirm TOTP enrolment");
    }

    audit_log::record(
        &state.db,
        audit_log::event::TOTP_ENROLLED,
        audit_log::Subjects::by(user.user_id),
        user_agent,
        None,
        audit_log::Change::none(),
        serde_json::json!({}),
    )
    .await;

    tracing::info!(user_id = %user.user_id, "TOTP enrolled");

    Json(TotpConfirmResponse { confirmed: true }).into_response()
}

/// Elevates the caller's own session for `STEP_UP_MINUTES`, given a code
/// from a confirmed authenticator app. Never issues a session and never
/// touches the cookie jar -- the caller is already signed in, or this
/// handler would not have been reached at all (see `AuthenticatedUser`).
pub async fn step_up(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Json(request): Json<TotpCodeRequest>,
) -> Response {
    let user_agent = crate::api::user_agent_from(&headers);

    if !totp_configured() {
        return not_configured();
    }

    let loaded = match load_own_confirmed_secret(&state, user.user_id).await {
        Ok(Some(loaded)) => loaded,
        Ok(None) => return not_enrolled(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "TOTP step-up lookup failed");
            return internal_error("Could not confirm this action");
        }
    };

    if loaded.is_locked {
        // Deliberately not counted as another failure: letting attempts
        // keep accruing while locked would extend the lock indefinitely
        // under a sustained attack, turning a 15-minute inconvenience into
        // a much longer one.
        tracing::warn!(user_id = %user.user_id, "TOTP step-up refused: locked out");
        return locked_out();
    }

    let matched_step = match verify_code(
        user.user_id,
        &loaded.secret_encrypted,
        &loaded.email,
        &request.code,
        loaded.last_used_step,
    ) {
        Ok(matched_step) => matched_step,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "could not verify a TOTP code");
            return internal_error("Could not confirm this action");
        }
    };

    let Some(matched_step) = matched_step else {
        let attempts: Result<i32, sqlx::Error> =
            sqlx::query_scalar("SELECT auth.record_totp_failure($1)")
                .bind(user.user_id)
                .fetch_one(&state.db)
                .await;

        match attempts {
            Ok(count) => {
                tracing::warn!(
                    user_id = %user.user_id,
                    failed_attempts = count,
                    "TOTP step-up rejected"
                );
                audit_log::record(
                    &state.db,
                    audit_log::event::TOTP_STEP_UP_FAILED,
                    audit_log::Subjects::by(user.user_id),
                    user_agent,
                    None,
                    audit_log::Change::none(),
                    serde_json::json!({
                        "failed_attempts": count,
                        // Correlates this attempt with a Phase II anomalous
                        // login (see auth_login.rs's assess_login_risk) --
                        // an operator reviewing the trail can tell "ordinary
                        // sensitive-action step-up rejected" apart from
                        // "someone couldn't clear a login-time anomaly gate".
                        "anomalous_login_pending_step_up": user.requires_step_up,
                    }),
                )
                .await;
            }
            Err(err) => {
                // The attempt is not counted, so log loudly: a brute-force
                // guard that silently stops counting is worse than none,
                // because nothing looks wrong.
                tracing::error!(error = %err, user_id = %user.user_id, "failed to record a TOTP failure");
            }
        }

        return wrong_code();
    };

    if let Err(err) = sqlx::query("SELECT auth.record_totp_success($1, $2)")
        .bind(user.user_id)
        .bind(matched_step)
        .execute(&state.db)
        .await
    {
        // The code was correct. Failing the request over the bookkeeping
        // would be the wrong trade -- but it does mean the failure counter
        // stays where it was, so this is `error`, not `warn`.
        tracing::error!(error = %err, user_id = %user.user_id, "failed to clear TOTP failure state");
    }

    let elevated: Result<bool, sqlx::Error> =
        sqlx::query_scalar("SELECT auth.record_step_up($1, $2)")
            .bind(&user.token_hash)
            .bind(STEP_UP_MINUTES)
            .fetch_one(&state.db)
            .await;

    match elevated {
        Ok(true) => {
            audit_log::record(
                &state.db,
                audit_log::event::TOTP_STEP_UP_SUCCEEDED,
                audit_log::Subjects::by(user.user_id),
                user_agent,
                None,
                audit_log::Change::none(),
                // Same correlation as the failure branch above -- true here
                // means this success is what cleared a pending login-time
                // anomaly gate (auth.record_step_up clears
                // sessions.requires_step_up unconditionally on success).
                serde_json::json!({ "anomalous_login_pending_step_up": user.requires_step_up }),
            )
            .await;

            tracing::info!(user_id = %user.user_id, "TOTP step-up succeeded");

            Json(StepUpResponse { confirmed: true }).into_response()
        }
        // The code was correct, but the session itself vanished (expired
        // or was revoked from another device) in the time it took to
        // verify it -- rare, but a stale success would be worse than
        // telling the truth: sign in again.
        Ok(false) => {
            tracing::warn!(user_id = %user.user_id, "TOTP step-up verified but the session was gone");
            unauthorized_session_gone()
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to record a TOTP step-up");
            internal_error("Could not confirm this action")
        }
    }
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

struct LoadedSecret {
    secret_encrypted: Vec<u8>,
    email: String,
    is_locked: bool,
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

/// Reads the caller's own *pending* (unconfirmed) TOTP secret -- the
/// candidate `/enroll/begin` wrote, not the live `secret_encrypted` a
/// confirmed step-up factor uses. `pending_secret_encrypted IS NOT NULL`
/// in the WHERE clause means "no enrolment in progress" reads the same
/// as "no row at all" -- both are `wrong_code()` to the confirm handler,
/// which is the correct response either way.
async fn load_own_secret(
    state: &AppState,
    user_id: Uuid,
) -> Result<Option<LoadedSecret>, sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(&state.db, user_id).await?;

    let row: Option<(Vec<u8>, String, bool)> = sqlx::query_as(
        "SELECT t.pending_secret_encrypted, u.email::text,
                (t.locked_until IS NOT NULL AND t.locked_until > now())
           FROM auth.totp_credentials t
           JOIN auth.users u ON u.id = t.user_id
          WHERE t.user_id = $1 AND t.pending_secret_encrypted IS NOT NULL",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(
        row.map(|(secret_encrypted, email, is_locked)| LoadedSecret {
            secret_encrypted,
            email,
            is_locked,
        }),
    )
}

struct LoadedConfirmedSecret {
    secret_encrypted: Vec<u8>,
    email: String,
    is_locked: bool,
    /// The TOTP step this credential last accepted a code at, if any. Fed
    /// straight into `verify_code`'s replay guard -- see auth::totp.
    last_used_step: Option<i64>,
}

/// Reads the caller's own TOTP secret for step-up verification --
/// `confirmed_at IS NOT NULL` only, unlike `load_own_secret`: an
/// unconfirmed enrolment (mid-setup, never proven to work) must not count
/// as a usable step-up factor.
async fn load_own_confirmed_secret(
    state: &AppState,
    user_id: Uuid,
) -> Result<Option<LoadedConfirmedSecret>, sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(&state.db, user_id).await?;

    let row: Option<(Vec<u8>, String, bool, Option<i64>)> = sqlx::query_as(
        "SELECT t.secret_encrypted, u.email::text,
                (t.locked_until IS NOT NULL AND t.locked_until > now()),
                t.last_used_step
           FROM auth.totp_credentials t
           JOIN auth.users u ON u.id = t.user_id
          WHERE t.user_id = $1 AND t.confirmed_at IS NOT NULL",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(row.map(
        |(secret_encrypted, email, is_locked, last_used_step)| LoadedConfirmedSecret {
            secret_encrypted,
            email,
            is_locked,
            last_used_step,
        },
    ))
}

/// Writes a fresh candidate secret into `pending_secret_encrypted`,
/// replacing any earlier unconfirmed candidate. Deliberately does NOT
/// touch `secret_encrypted`, `confirmed_at`, `failed_attempts`,
/// `locked_until`, or `last_used_at`/`last_used_step` -- an existing
/// confirmed secret (if any) must keep working as a step-up factor for
/// the entire re-enrolment window, not just until this call. See
/// `confirm_own_secret`, the only place `pending_secret_encrypted` is
/// ever promoted into the live column.
///
/// `ON CONFLICT` on the unique `user_id` rather than delete-then-insert,
/// so this is one statement and cannot leave the account with no row at
/// all if it fails halfway -- and it doubles as the first-enrolment path
/// too, since `secret_encrypted` is nullable specifically so a brand-new
/// row can exist with nothing confirmed yet.
async fn store_unconfirmed_secret(
    state: &AppState,
    user_id: Uuid,
    encrypted: &[u8],
) -> Result<(), sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(&state.db, user_id).await?;

    sqlx::query(
        "INSERT INTO auth.totp_credentials (user_id, pending_secret_encrypted)
         VALUES ($1, $2)
         ON CONFLICT (user_id) DO UPDATE
            SET pending_secret_encrypted = EXCLUDED.pending_secret_encrypted",
    )
    .bind(user_id)
    .bind(encrypted)
    .execute(&mut *tx)
    .await?;

    tx.commit().await
}

/// Promotes `pending_secret_encrypted` into `secret_encrypted` -- the one
/// and only moment a candidate becomes the live step-up factor, and the
/// one and only place an existing confirmed secret is ever replaced.
/// `matched_step` is the step the confirming code was accepted at --
/// recorded as `last_used_step` immediately, so the same code cannot be
/// replayed as a step-up the instant enrolment finishes (see auth::totp's
/// replay-window docs).
async fn confirm_own_secret(
    state: &AppState,
    user_id: Uuid,
    matched_step: i64,
) -> Result<(), sqlx::Error> {
    let mut tx = begin_owner_rls_transaction(&state.db, user_id).await?;

    sqlx::query(
        "UPDATE auth.totp_credentials
            SET secret_encrypted = pending_secret_encrypted,
                pending_secret_encrypted = NULL,
                confirmed_at = now(),
                failed_attempts = 0,
                locked_until = NULL,
                last_used_step = $2
          WHERE user_id = $1",
    )
    .bind(user_id)
    .bind(matched_step)
    .execute(&mut *tx)
    .await?;

    tx.commit().await
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

    /// Step-up is authenticated-only, so there is no anonymous caller to
    /// hide this from -- unlike the old login path, an honest 503 is
    /// correct here, the same as enrolment already does.
    #[tokio::test]
    #[serial(totp_env)]
    async fn an_unconfigured_deployment_says_so_on_the_step_up_path() {
        std::env::remove_var("TOTP_ENCRYPTION_KEY");

        let response = step_up(
            State(empty_state()),
            AuthenticatedUser {
                user_id: Uuid::new_v4(),
                role_keys: vec!["admin".to_string()],
                permission_keys: std::collections::HashSet::new(),
                token_hash: vec![0u8; 32],
                elevated_until: None,
                requires_step_up: false,
                passkey_reverified_until: None,
            },
            HeaderMap::new(),
            Json(TotpCodeRequest {
                code: "123456".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        set_key();
    }

    // step_up's "no confirmed TOTP" / "locked out" / "code verified,
    // session gone" branches all require a real database to distinguish
    // "no row" from "connection error" -- like enroll_confirm, they're
    // exercised against the real dev database rather than as a unit test
    // against test_support::empty_state()'s intentionally-lazy,
    // fails-fast-in-50ms pool (see that pool's own doc comment).

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
                role_keys: vec!["admin".to_string()],
                permission_keys: std::collections::HashSet::new(),
                token_hash: vec![0u8; 32],
                elevated_until: None,
                requires_step_up: false,
                passkey_reverified_until: None,
            },
            HeaderMap::new(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

        set_key();
    }
}
