use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Json, State},
    http::{HeaderMap, StatusCode},
};

use super::invite::*;
use super::recovery::*;
use crate::api::test_support::{admin_user, empty_state, onboarding_manager_user};

/// A stand-in peer address -- both handlers now take
/// `ConnectInfo<SocketAddr>` (only populated for real by
/// `into_make_service_with_connect_info` outside of tests), matching
/// the same fixture already used in auth_login.rs/auth_register.rs.
fn test_addr() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
}

fn request(email: &str, company: &str) -> CreateInviteRequest {
    CreateInviteRequest {
        email: email.to_string(),
        first_name: "Ada".to_string(),
        last_name: "Lovelace".to_string(),
        company: company.to_string(),
        job_title: None,
        role: "admin".to_string(),
    }
}

/// Every validation failure must be caught before the database is
/// touched. `empty_state`'s pool points at nothing reachable, so a query
/// surfaces as a 500 -- meaning a 400 here also proves no connection was
/// attempted, which is the property worth having: a typo should not cost
/// a transaction.
#[tokio::test]
async fn invalid_input_is_refused_without_touching_the_database() {
    let cases = [
        (request("", "quikstor"), "empty email"),
        (request("   ", "quikstor"), "whitespace email"),
        (request("not-an-address", "quikstor"), "no @ sign"),
        (
            request("a b@example.com", "quikstor"),
            "embedded whitespace",
        ),
        (request("ada@example.com", "acme"), "unknown company"),
        (request("ada@example.com", ""), "empty company"),
    ];

    for (body, label) in cases {
        let response = create_invite(
            State(empty_state()),
            admin_user(),
            test_addr(),
            HeaderMap::new(),
            Json(body),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "expected a 400 for {label}"
        );
    }
}

/// A missing name is as invalid as a missing email -- the invited person
/// has to be addressable in the authenticator prompt, which shows the
/// display name.
#[tokio::test]
async fn blank_names_are_refused() {
    let mut body = request("ada@example.com", "quikstor");
    body.first_name = "   ".to_string();

    let response = create_invite(
        State(empty_state()),
        admin_user(),
        test_addr(),
        HeaderMap::new(),
        Json(body),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// A non-admin authenticated caller must be refused before any
/// validation of the request body even runs -- permission-gating
/// happens first.
#[tokio::test]
async fn create_invite_refuses_insufficient_permission() {
    let response = create_invite(
        State(empty_state()),
        onboarding_manager_user(),
        test_addr(),
        HeaderMap::new(),
        Json(request("ada@example.com", "quikstor")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// Regression test: a caller holding `users.manage` but not
/// `users.manage_roles` must still be refused. Before this gate, such
/// a role -- e.g. a narrower "can invite people" role deliberately
/// scoped without the ability to grant admin -- could invite a
/// brand-new account straight in as `admin`, bypassing the entire
/// reason `users.manage_roles` exists as a separate permission from
/// `grant_role`/`revoke_role`'s own gate.
#[tokio::test]
async fn create_invite_refuses_users_manage_without_users_manage_roles() {
    let narrow_role_user = crate::auth::AuthenticatedUser {
        user_id: uuid::Uuid::new_v4(),
        role_keys: vec!["custom_inviter".to_string()],
        permission_keys: ["users.manage".to_string()].into_iter().collect(),
        token_hash: vec![0u8; 32],
        elevated_until: None,
        requires_step_up: false,
        passkey_reverified_until: None,
    };

    let response = create_invite(
        State(empty_state()),
        narrow_role_user,
        test_addr(),
        HeaderMap::new(),
        Json(request("ada@example.com", "quikstor")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// Company matching must not depend on the caller's capitalisation, and
/// the email is lowercased before it reaches a `citext` column so two
/// spellings of one address cannot become two accounts. Reaching the
/// database (a 500 against the unreachable test pool) is the *success*
/// signal here: it proves validation passed rather than rejecting a
/// legitimate request.
#[tokio::test]
async fn company_and_email_casing_are_normalised_not_rejected() {
    let response = create_invite(
        State(empty_state()),
        admin_user(),
        test_addr(),
        HeaderMap::new(),
        Json(request("Ada@Example.COM", "QuikStor")),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "validation should have passed and the call should have reached the database"
    );
}

/// An unknown role can no longer be caught before the database is
/// touched -- roles are real data now (see the module doc), so the
/// only source of truth is the `auth.roles` table itself, which this
/// handler only reaches inside a transaction. Reaching the database
/// (a 500 against the unreachable test pool) is the success signal
/// here: it proves every pre-transaction check passed and the role
/// lookup was actually attempted. The "no such role" 400 itself is
/// exercised against the real dev database, not this fake pool, same
/// as several other DB-dependent branches in this codebase.
#[tokio::test]
async fn an_unrecognised_role_still_reaches_the_database() {
    let mut body = request("ada@example.com", "quikstor");
    body.role = "superuser".to_string();

    let response = create_invite(
        State(empty_state()),
        admin_user(),
        test_addr(),
        HeaderMap::new(),
        Json(body),
    )
    .await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// A blank role is still caught before the database, same as the other
/// required-field checks -- there is no ambiguity to resolve against
/// `auth.roles` for an empty string.
#[tokio::test]
async fn a_blank_role_is_refused_without_touching_the_database() {
    let mut body = request("ada@example.com", "quikstor");
    body.role = "   ".to_string();

    let response = create_invite(
        State(empty_state()),
        admin_user(),
        test_addr(),
        HeaderMap::new(),
        Json(body),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

/// Any role key reaches the same code path -- reaching the database (a
/// 500 against the unreachable test pool) is the success signal, same
/// convention as the casing-normalisation test above.
#[tokio::test]
async fn onboarding_manager_is_an_accepted_role_string() {
    let mut body = request("ada@example.com", "quikstor");
    body.role = "onboarding_manager".to_string();

    let response = create_invite(
        State(empty_state()),
        admin_user(),
        test_addr(),
        HeaderMap::new(),
        Json(body),
    )
    .await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

fn recover_request(email: &str) -> RecoverAccountRequest {
    RecoverAccountRequest {
        email: email.to_string(),
    }
}

/// Same property as `invalid_input_is_refused_without_touching_the_database`
/// above, for the one field this endpoint takes.
#[tokio::test]
async fn recovery_rejects_an_invalid_email_without_touching_the_database() {
    let cases = [
        (recover_request(""), "empty email"),
        (recover_request("   "), "whitespace email"),
        (recover_request("not-an-address"), "no @ sign"),
        (recover_request("a b@example.com"), "embedded whitespace"),
    ];

    for (body, label) in cases {
        let response = recover_account(
            State(empty_state()),
            admin_user(),
            test_addr(),
            HeaderMap::new(),
            Json(body),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "expected a 400 for {label}"
        );
    }
}

/// Same permission-gating property as `create_invite_refuses_insufficient_permission`,
/// on the recovery endpoint.
#[tokio::test]
async fn recover_account_refuses_insufficient_permission() {
    let response = recover_account(
        State(empty_state()),
        onboarding_manager_user(),
        test_addr(),
        HeaderMap::new(),
        Json(recover_request("someone@example.com")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

/// A syntactically valid email must reach the database -- the 500 here
/// (against the unreachable test pool) is the success signal, same
/// convention as `company_and_email_casing_are_normalised_not_rejected`.
#[tokio::test]
async fn recovery_with_a_valid_email_reaches_the_database() {
    let response = recover_account(
        State(empty_state()),
        admin_user(),
        test_addr(),
        HeaderMap::new(),
        Json(recover_request("someone@example.com")),
    )
    .await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}
