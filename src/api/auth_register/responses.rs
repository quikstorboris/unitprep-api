//! The refusal and failure responses, and the audit row every refusal writes.

use crate::api::{bad_request, error_response, AppState};
use crate::auth::audit_log;
use axum::http::StatusCode;
use axum::response::Response;
use uuid::Uuid;

/// Deliberately identical for "no such invite", "invite expired",
/// "invite already used", "user no longer invited", and "user already has
/// a passkey". Distinguishing them would turn this unauthenticated
/// endpoint into an oracle -- and the cases that arguably aren't secrets
/// aren't worth carving out, since carving them out is precisely what
/// reveals the others by elimination.
pub(super) fn registration_unavailable() -> Response {
    error_response(
        StatusCode::FORBIDDEN,
        "registration_not_available",
        "Passkey registration is not available for this account.".to_string(),
    )
}

/// The rejection above, plus the audit row that makes it visible to an
/// operator.
///
/// Every rejection path goes through here rather than calling
/// `registration_unavailable` directly, so "refused but recorded nowhere"
/// cannot be reintroduced by adding another reason later and forgetting
/// the audit call. `reason` lands in the audit table and never in the
/// response.
///
/// **Nothing derived from the invite token is recorded** -- not the raw
/// value, not its hash. A token is a bearer credential; an audit trail
/// containing live tokens is a credential store with a different name on
/// it. The reason alone is what an operator needs, and the deliberate
/// consequence is that a refused attempt with an unrecognised token
/// identifies no user, because there genuinely is none to identify.
///
/// `actor_user_id` is passed through rather than always `None`: once a
/// ceremony has resolved to a user, later failures for that ceremony can
/// name them honestly, and an audit row that can be joined to a user is
/// worth considerably more than one that cannot.
pub(super) async fn reject_registration(
    state: &AppState,
    reason: &'static str,
    actor_user_id: Option<Uuid>,
    user_agent: Option<&str>,
    ip_address: Option<sqlx::types::ipnetwork::IpNetwork>,
) -> Response {
    // `warn`, matching a failed login ceremony: an ordinary client-side
    // outcome, not a server fault.
    tracing::warn!(reason, "passkey registration refused");

    audit_log::record(
        &state.db,
        audit_log::event::REGISTRATION_FAILED,
        // Whatever the caller resolved to, which is `None` on the paths that
        // refused before any user was identified. Never a target: nobody had
        // anything done *to* them here.
        audit_log::Subjects {
            actor: actor_user_id,
            target: None,
        },
        user_agent,
        ip_address,
        audit_log::Change::none(),
        serde_json::json!({ "reason": reason }),
    )
    .await;

    registration_unavailable()
}

pub(super) fn ceremony_not_found() -> Response {
    bad_request(
        "ceremony_not_found",
        "This registration attempt has expired or was never started. Start again.".to_string(),
    )
}

pub(super) fn ceremony_failed() -> Response {
    bad_request(
        "registration_failed",
        "The passkey could not be verified. Start again.".to_string(),
    )
}
