use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Json, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use axum_extra::extract::cookie::CookieJar;
use uuid::Uuid;

use super::dto::{RegisterBeginRequest, RegisterFinishRequest};
use super::{register_begin, register_finish};
use crate::api::test_support::empty_state;
use crate::auth::{issue_ceremony_cookie, RegistrationCeremony, REGISTRATION_CEREMONY_COOKIE};

/// A stand-in peer address for tests -- `register_finish` now takes
/// `ConnectInfo<SocketAddr>` (only populated for real by
/// `into_make_service_with_connect_info` outside of tests). The actual
/// value never matters here: every test below fails before reaching
/// create_session, against the unreachable test pool.
fn test_addr() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
}

/// An unauthenticated caller with no invite token has nothing that
/// could authorize a registration, and must be refused without the
/// database being consulted at all.
///
/// Replaces two earlier tests that asserted the `AUTH_BOOTSTRAP_ENABLED`
/// env gate held shut. That gate is gone: there is no env var to leave
/// unset, because there is no longer an unauthenticated path that a
/// deployment could accidentally open. Possession of an unguessable
/// token is now the only authorization, which is a stronger property
/// than a correctly-configured flag.
#[tokio::test]
async fn begin_refuses_an_unauthenticated_caller_without_an_invite_token() {
    // `empty_state`'s pool is lazy and points at nothing reachable, so
    // a 403 here also proves no lookup was attempted -- a query would
    // surface as a 500.
    let response = register_begin(
        State(empty_state()),
        test_addr(),
        CookieJar::new(),
        HeaderMap::new(),
        Json(RegisterBeginRequest { invite_token: None }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// The env var that used to gate this path must not still be consulted
/// anywhere. Setting it to the value that previously opened the gate
/// must now change nothing whatsoever.
///
/// Written because "delete the gate" is easy to do incompletely: a
/// leftover read in one branch would restore an unauthenticated path
/// that no test named, and the vault's standing instruction was that
/// this variable be *deleted*, not merely unset.
#[tokio::test]
async fn the_old_bootstrap_env_var_no_longer_opens_anything() {
    std::env::set_var("AUTH_BOOTSTRAP_ENABLED", "true");

    let response = register_begin(
        State(empty_state()),
        test_addr(),
        CookieJar::new(),
        HeaderMap::new(),
        Json(RegisterBeginRequest { invite_token: None }),
    )
    .await;

    std::env::remove_var("AUTH_BOOTSTRAP_ENABLED");

    assert_eq!(
        response.status(),
        StatusCode::FORBIDDEN,
        "AUTH_BOOTSTRAP_ENABLED must be inert -- if this fails, a read of it survived the removal"
    );
}

/// Every rejection reason must produce a byte-identical response.
/// This is the anti-enumeration property, and it is exactly what the
/// new audit rows could have broken -- recording a distinct `reason`
/// server-side is only safe while none of it reaches the caller. The
/// body is compared, not just the status: a `reason` leaking into the
/// error payload would keep the status at 403 and still hand an
/// attacker the oracle.
#[tokio::test]
async fn every_rejection_reason_returns_an_identical_response() {
    async fn body_of(response: Response) -> (StatusCode, Vec<u8>) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        (status, bytes.to_vec())
    }

    // No token at all -- refused before any lookup.
    let no_token = body_of(
        register_begin(
            State(empty_state()),
            test_addr(),
            CookieJar::new(),
            HeaderMap::new(),
            Json(RegisterBeginRequest { invite_token: None }),
        )
        .await,
    )
    .await;

    // Whitespace-only, which trims to nothing and takes the same
    // branch.
    let blank_token = body_of(
        register_begin(
            State(empty_state()),
            test_addr(),
            CookieJar::new(),
            HeaderMap::new(),
            Json(RegisterBeginRequest {
                invite_token: Some("   ".to_string()),
            }),
        )
        .await,
    )
    .await;

    assert_eq!(no_token.0, StatusCode::FORBIDDEN);
    assert_eq!(
        no_token, blank_token,
        "a blank token must not be distinguishable from a missing one"
    );
}

/// The invite token must never reach the response, in any form. A
/// token echoed back into an error body would be a bearer credential
/// in a place that gets logged by proxies, screenshotted, and pasted
/// into bug reports.
#[tokio::test]
async fn a_refusal_never_echoes_the_invite_token() {
    let invite_token = "not-a-real-token-abc123";

    let response = register_begin(
        State(empty_state()),
        test_addr(),
        CookieJar::new(),
        HeaderMap::new(),
        Json(RegisterBeginRequest {
            invite_token: Some(invite_token.to_string()),
        }),
    )
    .await;

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body should be readable");
    let body = String::from_utf8_lossy(&bytes);

    assert!(
        !body.contains(invite_token),
        "the refusal body must not contain the submitted token: {body}"
    );
}

/// No cookie means there is no ceremony to finish -- and critically,
/// that must not surface as a verification failure or a 500.
#[tokio::test]
async fn finish_without_a_ceremony_cookie_is_a_bad_request() {
    let response = register_finish(
        State(empty_state()),
        test_addr(),
        CookieJar::new(),
        HeaderMap::new(),
        Json(RegisterFinishRequest {
            credential: serde_json::json!({}),
            nickname: None,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// A cookie naming a ceremony the store has never heard of (expired,
/// process restarted, or simply fabricated) is the same "start
/// again" case as no cookie at all.
#[tokio::test]
async fn finish_with_an_unknown_ceremony_id_is_a_bad_request() {
    let jar = issue_ceremony_cookie(
        CookieJar::new(),
        REGISTRATION_CEREMONY_COOKIE,
        "not-a-real-ceremony".to_string(),
        time::Duration::minutes(5),
    );

    let response = register_finish(
        State(empty_state()),
        test_addr(),
        jar,
        HeaderMap::new(),
        Json(RegisterFinishRequest {
            credential: serde_json::json!({}),
            nickname: None,
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// Regression guard for the single-use property: a ceremony must be
/// gone from the store after one `/finish` attempt, INCLUDING a
/// failing one, so the same challenge can never be retried. Written
/// against a failing attempt specifically because that is the case a
/// "delete on success" implementation would get wrong while still
/// looking correct in the happy path.
#[tokio::test]
async fn a_failed_finish_still_consumes_the_ceremony() {
    let state = empty_state();
    let user_id = Uuid::new_v4();

    state
        .registration_ceremonies
        .save(RegistrationCeremony::new(
            "ceremony-1".to_string(),
            user_id,
            b"not-valid-webauthn-state".to_vec(),
            Some(b"invite-token-hash".to_vec()),
        ));

    let jar = issue_ceremony_cookie(
        CookieJar::new(),
        REGISTRATION_CEREMONY_COOKIE,
        "ceremony-1".to_string(),
        time::Duration::minutes(5),
    );

    let response = register_finish(
        State(state.clone()),
        test_addr(),
        jar,
        HeaderMap::new(),
        Json(RegisterFinishRequest {
            credential: serde_json::json!({ "garbage": true }),
            nickname: None,
        }),
    )
    .await;

    // Verification fails (the stored state is nonsense), but the
    // ceremony must be consumed regardless.
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    assert!(
        state
            .registration_ceremonies
            .get_handle("ceremony-1")
            .is_none(),
        "a consumed ceremony must not remain in the store"
    );
}
