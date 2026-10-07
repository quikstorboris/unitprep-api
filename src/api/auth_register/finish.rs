//! `POST /auth/register/finish` -- verifies the authenticator's response, enrols the credential, and on the invite path signs the user in.

use super::dto::{RegisterFinishRequest, RegisterFinishResponse};
use super::enrol::enrol_credential;
use super::responses::{ceremony_failed, ceremony_not_found, registration_unavailable};
use crate::api::{internal_error, AppState};
use crate::auth::{
    audit_log, clear_ceremony_cookie, generate_token, issue_session_cookie, read_ceremony_cookie,
    session_lifetime_hours, REGISTRATION_CEREMONY_COOKIE,
};
use axum::extract::{ConnectInfo, Json, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum_extra::extract::cookie::CookieJar;
use std::net::SocketAddr;
use unitprep_core::session_store::SessionStoreExt;
use uuid::Uuid;

pub async fn register_finish(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(request): Json<RegisterFinishRequest>,
) -> Response {
    // Read up front rather than at the point of the success audit row:
    // the failure path below needs it too, and a value extracted twice is
    // a value that eventually gets extracted differently in one of the
    // two places.
    let (user_agent, ip_address) = crate::api::request_context(&headers, addr);

    let Some(ceremony_id) = read_ceremony_cookie(&jar, REGISTRATION_CEREMONY_COOKIE) else {
        return ceremony_not_found();
    };

    // Read what's needed and release the lock before any `.await` --
    // holding a session lock across an await point is explicitly
    // forbidden by `SessionStore`'s documented locking invariants, and
    // everything below this point is async.
    let Some((user_id, correlation_id, webauthn_state, invite_token_hash)) = state
        .registration_ceremonies
        .with_session(&ceremony_id, |ceremony| {
            (
                ceremony.user_id,
                ceremony.correlation_id,
                ceremony.webauthn_state.clone(),
                ceremony.invite_token_hash.clone(),
            )
        })
    else {
        return (
            clear_ceremony_cookie(jar, REGISTRATION_CEREMONY_COOKIE),
            ceremony_not_found(),
        )
            .into_response();
    };

    // Single-use, and consumed BEFORE verification rather than after: a
    // failed or replayed attempt must not get a second try against the
    // same challenge. A legitimate retry needs a fresh `/begin`.
    state.registration_ceremonies.delete(&ceremony_id);

    let jar = clear_ceremony_cookie(jar, REGISTRATION_CEREMONY_COOKIE);

    let stored = match state
        .auth_backend
        .finish_registration(request.credential, &webauthn_state)
    {
        Ok(stored) => stored,
        Err(err) => {
            // `warn`, not `error`: a failed ceremony is an ordinary
            // client-side outcome (user cancelled, wrong device, stale
            // challenge), not a server fault.
            tracing::warn!(
                user_id = %user_id,
                correlation_id = %correlation_id,
                error = %err,
                "passkey registration ceremony failed verification"
            );

            // The registration-side counterpart of login's
            // `assertion_rejected` row. Without it, a ceremony that
            // started and then failed verification appeared in the ops
            // log and nowhere permanent -- the same gap as the refused
            // `/begin`, one step further along.
            audit_log::record(
                &state.db,
                audit_log::event::REGISTRATION_FAILED,
                audit_log::Subjects::by(user_id),
                user_agent,
                ip_address,
                audit_log::Change::none(),
                serde_json::json!({
                    "reason": "credential_rejected",
                    "correlation_id": correlation_id,
                }),
            )
            .await;

            return (jar, ceremony_failed()).into_response();
        }
    };

    let is_invite = invite_token_hash.is_some();

    match enrol_credential(
        &state,
        user_id,
        &stored,
        request.nickname.as_deref(),
        invite_token_hash.as_deref(),
    )
    .await
    {
        Ok(true) => {}

        // The credential verified, but the invite was no longer consumable
        // by the time the transaction ran -- it expired mid-ceremony, or a
        // concurrent attempt used it first. Nothing was written: the
        // transaction rolled back, so the user is still `invited` with no
        // credential and can retry with a fresh invite.
        Ok(false) => {
            tracing::warn!(
                user_id = %user_id,
                correlation_id = %correlation_id,
                "invite was no longer consumable when the credential was ready"
            );

            audit_log::record(
                &state.db,
                audit_log::event::REGISTRATION_FAILED,
                audit_log::Subjects::by(user_id),
                user_agent,
                ip_address,
                audit_log::Change::none(),
                serde_json::json!({
                    "reason": "invite_consumed_elsewhere",
                    "correlation_id": correlation_id,
                }),
            )
            .await;

            return (jar, registration_unavailable()).into_response();
        }

        Err(err) => {
            tracing::error!(
                error = %err,
                user_id = %user_id,
                correlation_id = %correlation_id,
                "failed to persist passkey credential"
            );
            return (jar, internal_error("Could not save the passkey")).into_response();
        }
    }

    audit_log::record(
        &state.db,
        audit_log::event::PASSKEY_REGISTERED,
        audit_log::Subjects::by(user_id),
        user_agent,
        ip_address,
        audit_log::Change::none(),
        // `device_bound` is captured here rather than left to be read off
        // the credential row later. The flag exists purely for admin
        // visibility, and what an admin wants to know is what the
        // authenticator claimed *at enrolment* -- a value re-read from the
        // row months later cannot distinguish "enrolled as synced" from
        // "row edited since".
        serde_json::json!({
            "invite": is_invite,
            "device_bound": stored.device_bound,
            "correlation_id": correlation_id,
        }),
    )
    .await;

    if !is_invite {
        tracing::info!(
            user_id = %user_id,
            correlation_id = %correlation_id,
            "additional passkey registered"
        );

        return (
            jar,
            Json(RegisterFinishResponse {
                success: true,
                session_issued: false,
            }),
        )
            .into_response();
    }

    // Invite path only: it ends with the user signed in, which is what
    // makes accepting an invitation a single continuous act rather than
    // "enrol, then go and sign in separately". The invite is already
    // consumed and the account already `active` by this point, which is
    // exactly why `create_session` below can succeed -- it requires an
    // active user.
    let (raw_token, token_hash) = generate_token();

    let lifetime_hours = session_lifetime_hours();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(lifetime_hours);

    // requires_step_up is always false here -- this is a brand-new
    // enrolment, not a login with session history to compare against, so
    // the Phase II anomaly signal (see auth_login.rs's assess_login_risk)
    // has nothing to evaluate. ip_address is captured for real now: the
    // into_make_service_with_connect_info wiring this comment used to wait
    // on already exists in main.rs (added for the auth rate limiter), and
    // direct exposure (no reverse proxy) is this deployment's actual
    // topology today, so the raw peer address is trustworthy without a
    // forwarded-header policy. Revisit if a reverse proxy/CDN is ever put
    // in front of this service.
    let created: Result<Uuid, sqlx::Error> =
        sqlx::query_scalar("SELECT auth.create_session($1, $2, $3, $4, $5, false)")
            .bind(user_id)
            .bind(&token_hash)
            .bind(expires_at)
            .bind(sqlx::types::ipnetwork::IpNetwork::from(addr.ip()))
            .bind(user_agent)
            .fetch_one(&state.db)
            .await;

    match created {
        Ok(session_id) => {
            tracing::info!(
                user_id = %user_id,
                session_id = %session_id,
                correlation_id = %correlation_id,
                "invite accepted, passkey registered and session issued"
            );

            let jar = issue_session_cookie(jar, raw_token, time::Duration::hours(lifetime_hours));

            (
                jar,
                Json(RegisterFinishResponse {
                    success: true,
                    session_issued: true,
                }),
            )
                .into_response()
        }

        Err(err) => {
            // The credential IS saved at this point. Report the partial
            // outcome honestly rather than a flat failure that would
            // invite the user to re-register -- which would now hit the
            // "already has a credential" guard and look permanently
            // broken. Signing in via the normal login flow (task 5)
            // works; nothing needs redoing.
            tracing::error!(
                error = %err,
                user_id = %user_id,
                correlation_id = %correlation_id,
                "passkey saved but session creation failed"
            );

            (
                jar,
                internal_error("Passkey saved, but sign-in failed — try signing in"),
            )
                .into_response()
        }
    }
}
