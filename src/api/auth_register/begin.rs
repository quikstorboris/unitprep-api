//! `POST /auth/register/begin` -- decides who a ceremony is for (a signed-in caller, or an invite holder) and starts it.

use super::dto::{RegisterBeginRequest, RegisterBeginResponse};
use super::responses::reject_registration;
use super::CEREMONY_TTL_MINUTES;
use crate::api::{internal_error, AppState};
use crate::auth::{
    action_requires_step_up, begin_rls_transaction, hash_token, issue_ceremony_cookie,
    step_up_required, try_authenticated_user, RegistrationCeremony, ADD_PASSKEY,
    REGISTRATION_CEREMONY_COOKIE,
};
use axum::extract::{ConnectInfo, Json, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::CookieJar;
use std::net::SocketAddr;
use uuid::Uuid;

/// Who a ceremony is being started for, plus the names WebAuthn shows in
/// the authenticator's own UI.
pub(super) struct RegistrationTarget {
    pub(super) user_id: Uuid,
    pub(super) username: String,
    pub(super) display_name: String,

    /// Raw credential ids the authenticator should refuse to duplicate.
    /// Always empty on the invite path -- `resolve_invite_registration`
    /// only ever matches users with none, so there is nothing to exclude
    /// by construction.
    pub(super) exclude: Vec<Vec<u8>>,

    /// `Some` on the invite path, carrying the hash to be consumed at
    /// `finish`. `None` for an authenticated caller adding a passkey.
    pub(super) invite_token_hash: Option<Vec<u8>>,
}

pub async fn register_begin(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(request): Json<RegisterBeginRequest>,
) -> Response {
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    // Resolved ONCE. Asking twice (a second call to decide
    // which path this is) would both waste a round trip and open a window
    // where the two answers disagree -- the path a ceremony is
    // authorized under must be a single decision, not two independent
    // lookups that happen to usually agree.
    let authenticated = try_authenticated_user(&jar, &state).await;

    let target = match authenticated {
        Some(user) => {
            // Adding a passkey to an account that already has one is
            // exactly the kind of sensitive, high-blast-radius action
            // step-up exists for -- a hijacked session cookie alone must
            // not be enough to plant a durable new credential. The
            // invite path below needs no equivalent check: possession of
            // the (unguessable) token *is* its authorization, and there
            // is no existing session to step up.
            //
            // Gated by admin-configurable policy
            // (auth.auth_configuration.step_up_actions) rather than a
            // hardcoded `true`, so which self-service actions require
            // step-up can be tuned without a code change -- see
            // auth::step_up_policy.
            let requires_step_up =
                match action_requires_step_up(&state.db, user.user_id, ADD_PASSKEY).await {
                    Ok(requires_step_up) => requires_step_up,
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            user_id = %user.user_id,
                            "failed to read step-up policy for add_passkey"
                        );
                        return internal_error("Could not start passkey registration");
                    }
                };

            if requires_step_up && !user.is_elevated() {
                return step_up_required();
            }

            match authenticated_target(&state, user.user_id, &user.role_keys).await {
                Ok(Some(target)) => target,
                // A session that resolved but whose user row is missing or
                // invisible is a real inconsistency, not a bad request --
                // `resolve_session` already vouched for that user being
                // active and non-deleted.
                Ok(None) => return internal_error("Could not load the signed-in user"),
                Err(err) => {
                    tracing::error!(error = %err, "failed to load authenticated registration target");
                    return internal_error("Could not load the signed-in user");
                }
            }
        }

        None => {
            let Some(raw_token) = request
                .invite_token
                .as_deref()
                .map(str::trim)
                .filter(|token| !token.is_empty())
            else {
                return reject_registration(
                    &state,
                    "missing_invite_token",
                    None,
                    user_agent,
                    ip_address,
                )
                .await;
            };

            // Hashed immediately, and only the hash travels any further --
            // into the lookup, into the ceremony, and eventually into
            // `consume_invite`. The raw token exists solely for the length
            // of this scope. Same discipline as the session cookie: the
            // database stores hashes, so nothing else should hold the
            // plaintext either.
            let token_hash = hash_token(raw_token);

            match invite_target(&state, &token_hash).await {
                Ok(Some(target)) => target,
                Ok(None) => {
                    return reject_registration(
                        &state,
                        "invite_not_usable",
                        None,
                        user_agent,
                        ip_address,
                    )
                    .await
                }
                Err(err) => {
                    tracing::error!(error = %err, "invite registration lookup failed");
                    return internal_error("Could not start passkey registration");
                }
            }
        }
    };

    let is_invite = target.invite_token_hash.is_some();

    let challenge = match state.auth_backend.start_registration(
        target.user_id,
        &target.username,
        &target.display_name,
        &target.exclude,
    ) {
        Ok(challenge) => challenge,
        Err(err) => {
            tracing::error!(error = %err, "failed to start passkey registration ceremony");
            return internal_error("Could not start passkey registration");
        }
    };

    let ceremony_id = Uuid::new_v4().to_string();

    let ceremony = RegistrationCeremony::new(
        ceremony_id.clone(),
        target.user_id,
        challenge.state,
        target.invite_token_hash,
    );

    // Read before the store takes ownership. Logged in place of
    // `ceremony_id`, which is the cookie's own value -- see
    // `RegistrationCeremony::correlation_id`.
    let correlation_id = ceremony.correlation_id;

    state.registration_ceremonies.save(ceremony);

    let jar = issue_ceremony_cookie(
        jar,
        REGISTRATION_CEREMONY_COOKIE,
        ceremony_id,
        time::Duration::minutes(CEREMONY_TTL_MINUTES),
    );

    tracing::info!(
        user_id = %target.user_id,
        correlation_id = %correlation_id,
        is_invite,
        "passkey registration ceremony started"
    );

    (
        jar,
        Json(RegisterBeginResponse {
            challenge: challenge.challenge,
        }),
    )
        .into_response()
}

/// The signed-in caller's own row plus their existing credential ids.
/// Runs inside an RLS transaction under their own identity, so the
/// database enforces "own row only" independently of this query's own
/// WHERE clause -- the WHERE is not the security boundary here.
pub(super) async fn authenticated_target(
    state: &AppState,
    user_id: Uuid,
    role_keys: &[String],
) -> Result<Option<RegistrationTarget>, sqlx::Error> {
    let mut tx = begin_rls_transaction(&state.db, user_id, role_keys).await?;

    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT email::text, first_name, last_name FROM auth.users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;

    let Some((email, first_name, last_name)) = row else {
        tx.rollback().await?;
        return Ok(None);
    };

    let exclude: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT credential_id FROM auth.webauthn_credentials WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_all(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Some(RegistrationTarget {
        user_id,
        username: email,
        display_name: format!("{first_name} {last_name}"),
        exclude,
        // No invite involved: this caller already has a session, so there
        // is nothing to consume and nothing to activate.
        invite_token_hash: None,
    }))
}

/// The unauthenticated first-passkey path. Every authorization check
/// lives inside `auth.resolve_invite_registration` (invite unused and
/// unexpired, user still `invited`, zero existing credentials) -- a `None`
/// here means "not usable", with the specific reason deliberately not
/// reported back to the caller.
///
/// The email comes back from the lookup rather than from the request: the
/// caller supplies only a token, and the address WebAuthn shows in the
/// authenticator's own prompt must be the invited account's real one, not
/// anything the client could influence.
pub(super) async fn invite_target(
    state: &AppState,
    token_hash: &[u8],
) -> Result<Option<RegistrationTarget>, sqlx::Error> {
    let row: Option<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT user_id, email, first_name, last_name
           FROM auth.resolve_invite_registration($1)",
    )
    .bind(token_hash)
    .fetch_optional(&state.db)
    .await?;

    Ok(row.map(
        |(user_id, email, first_name, last_name)| RegistrationTarget {
            user_id,
            username: email,
            display_name: format!("{first_name} {last_name}"),
            exclude: Vec::new(),
            invite_token_hash: Some(token_hash.to_vec()),
        },
    ))
}
