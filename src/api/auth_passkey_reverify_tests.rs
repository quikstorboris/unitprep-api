use axum::http::StatusCode;
use axum_extra::extract::cookie::CookieJar;

use super::*;
use crate::api::test_support::{empty_state, test_user};
use crate::auth::AuthenticationCeremony;

#[tokio::test]
async fn finish_returns_ceremony_not_found_with_no_cookie_at_all() {
    let response = reverify_finish(
        State(empty_state()),
        test_user(),
        HeaderMap::new(),
        CookieJar::new(),
        Json(ReverifyFinishRequest {
            credential: serde_json::json!({}),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn finish_returns_ceremony_not_found_for_an_unknown_ceremony_id() {
    let jar = issue_ceremony_cookie(
        CookieJar::new(),
        REVERIFY_CEREMONY_COOKIE,
        "does-not-exist".to_string(),
        time::Duration::minutes(5),
    );

    let response = reverify_finish(
        State(empty_state()),
        test_user(),
        HeaderMap::new(),
        jar,
        Json(ReverifyFinishRequest {
            credential: serde_json::json!({}),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// Regression test for the cross-user ceremony check: a ceremony begun
/// under one user_id must not be completable by a different caller, even
/// with a genuine ceremony cookie in hand -- e.g. a different account
/// signed in on the same browser mid-ceremony. This resolves before any
/// database call, so it is safe to exercise without a real pool.
#[tokio::test]
async fn finish_refuses_a_ceremony_started_under_a_different_user() {
    let state = empty_state();
    let caller = test_user();

    let ceremony_id = "ceremony-1".to_string();
    let ceremony = AuthenticationCeremony::new(
        ceremony_id.clone(),
        uuid::Uuid::new_v4(), // a different user than `caller`
        Vec::new(),
    );
    state.authentication_ceremonies.save(ceremony);

    let jar = issue_ceremony_cookie(
        CookieJar::new(),
        REVERIFY_CEREMONY_COOKIE,
        ceremony_id.clone(),
        time::Duration::minutes(5),
    );

    let response = reverify_finish(
        State(state.clone()),
        caller,
        HeaderMap::new(),
        jar,
        Json(ReverifyFinishRequest {
            credential: serde_json::json!({}),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Single-use: consumed regardless of which user asked to finish it.
    assert!(state
        .authentication_ceremonies
        .get_handle(&ceremony_id)
        .is_none());
}
