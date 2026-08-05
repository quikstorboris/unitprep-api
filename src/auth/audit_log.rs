//! Append-only audit-event recording.
//!
//! The build plan calls for audit capture wired in from day one rather
//! than switched on later, so there is no gap in the record. This is the
//! one place that writes `auth.auth_audit_logs`.
//!
//! ## Never use `INSERT ... RETURNING` here
//!
//! `RETURNING` requires the newly inserted row to be *readable*, so it is
//! evaluated against `auth_audit_logs_select_admin_only`. The insert
//! therefore succeeds or fails depending on whether `app.current_user_role`
//! happens to be `'admin'` at the time:
//!
//! | statement | identity context | result |
//! |---|---|---|
//! | `INSERT ... RETURNING id` | none | **ERROR** |
//! | `INSERT ... RETURNING id` | `role = 'admin'` | ok |
//! | `INSERT ...` (no RETURNING) | none | ok |
//!
//! The error is `new row violates row-level security policy`, which points
//! at the wrong thing entirely -- the `WITH CHECK (true)` insert policy
//! passed fine; it is reading the returned row that fails.
//!
//! What makes this worth a module-level warning is that the failure is
//! *selective*: logging would work for admin-context events and break on
//! precisely the events that have no identity context -- failed logins --
//! which are the ones the audit trail exists for. Verified all four
//! combinations as `app_service` against the dev branch 2026-07-29.
//!
//! So: no `RETURNING`, and no GUC context is set up before writing. The
//! insert policy is unconditional by design specifically so that an event
//! with no known actor still lands.

use serde_json::Value;
use sqlx::types::ipnetwork::IpNetwork;
use sqlx::PgPool;
use uuid::Uuid;

/// Event types written so far. A plain `&'static str` rather than an enum
/// mapped to a Postgres type, matching the schema's deliberate choice to
/// keep `event_type` as `text`: this set grows new categories often, and
/// `ALTER TYPE ... ADD VALUE` on a frequently-extended column is friction
/// for no benefit. Kept as constants so a typo is a compile error at the
/// call site rather than a silently unqueryable row.
pub mod event {
    pub const LOGIN_SUCCEEDED: &str = "login_succeeded";
    pub const LOGIN_FAILED: &str = "login_failed";
    pub const PASSKEY_REGISTERED: &str = "passkey_registered";

    /// The registration-side counterpart of `LOGIN_FAILED`, covering both
    /// a refused `/register/begin` and a `/register/finish` whose
    /// credential did not verify. It exists because its absence was an
    /// asymmetry rather than a decision: probing login across a list of
    /// addresses was recorded and probing registration was not, so one
    /// class of the same attack was invisible to the operator. The HTTP
    /// response stays indistinguishable to the caller either way -- what
    /// the attacker cannot tell apart and what the operator cannot see
    /// are separate properties, and only the first one is deliberate.
    pub const REGISTRATION_FAILED: &str = "registration_failed";

    /// An administrator issued an invitation. The first event in this set
    /// where the actor and the subject are *different people*, which is
    /// what `target_user_id` exists for -- see `Subjects`.
    pub const INVITE_CREATED: &str = "invite_created";

    /// One or more sessions were revoked. `metadata.scope` distinguishes
    /// signing out of the current session from signing out everywhere, and
    /// `metadata.revoked_count` says how many rows it reached -- the pair
    /// answers "did this person sign out, or did they sign out because
    /// something was wrong", which one session id alone cannot.
    pub const SESSION_REVOKED: &str = "session_revoked";

    /// A user began TOTP enrolment: a secret was written but not yet proven.
    /// Recorded separately from `TOTP_ENROLLED` because the gap between the
    /// two is where a mis-scanned secret lives, and a begin with no matching
    /// enrolled event is the signature of exactly that.
    pub const TOTP_ENROLMENT_STARTED: &str = "totp_enrolment_started";

    /// A confirmation code was rejected during enrolment. Distinct from
    /// `LOGIN_FAILED`: nobody was trying to sign in, so counting it as a
    /// failed login would inflate that number with ordinary setup fumbles.
    pub const TOTP_ENROLMENT_FAILED: &str = "totp_enrolment_failed";

    /// TOTP is confirmed and usable as a step-up factor for sensitive
    /// in-session actions (see TOTP_STEP_UP_SUCCEEDED) -- not a way to log
    /// in; see auth_totp.rs's module docs.
    pub const TOTP_ENROLLED: &str = "totp_enrolled";

    /// An administrator attempted to issue an invitation to an account
    /// that was not eligible -- already credentialed, or not in `invited`
    /// status. Distinct from `INVITE_CREATED` for the same reason
    /// `REGISTRATION_FAILED` is distinct from `PASSKEY_REGISTERED`: a
    /// refusal and a success are different facts, and collapsing them
    /// into "something happened to this invite" would hide which one it
    /// was.
    pub const INVITE_REFUSED: &str = "invite_refused";

    /// An administrator initiated account recovery: every existing access
    /// path on the target account (passkeys, TOTP, live sessions, any
    /// outstanding invite) was revoked and a fresh invite issued in its
    /// place. Distinct from `INVITE_CREATED` -- this is materially more
    /// consequential, since unlike an ordinary invite it destroys existing
    /// credentials rather than only adding a path for someone who never
    /// had one. Kept as its own event rather than `INVITE_CREATED` with a
    /// flag so a review of "who got their credentials revoked and why"
    /// never has to filter a mixed-purpose event to find out.
    pub const ACCOUNT_RECOVERY_INITIATED: &str = "account_recovery_initiated";

    /// An already-signed-in caller proved a fresh TOTP code, elevating
    /// their own session for a short window (auth.record_step_up) so a
    /// sensitive action (adding/replacing a passkey) can proceed. Distinct
    /// from `LOGIN_SUCCEEDED`: nobody signed in here, a session already
    /// existed -- this is a re-proof of identity mid-session, the same
    /// distinction `TOTP_ENROLMENT_FAILED` draws against `LOGIN_FAILED`.
    pub const TOTP_STEP_UP_SUCCEEDED: &str = "totp_step_up_succeeded";

    /// A step-up code was rejected. Counted toward the same lockout as an
    /// enrolment/legacy-login rejection would have been (`failed_attempts`
    /// on the credential row) -- repeated guessing is repeated guessing
    /// regardless of which action prompted it.
    pub const TOTP_STEP_UP_FAILED: &str = "totp_step_up_failed";

    /// A successful login came from an IP address or user_agent never seen
    /// before for an account that has prior session history (Phase II
    /// anomaly signal, see auth_login.rs's `assess_login_risk`). Recorded
    /// unconditionally, even when `metadata.step_up_required` is false
    /// because the account has no TOTP confirmed to gate with -- an
    /// operator reviewing the trail should be able to see every anomalous
    /// login, not just the ones that happened to be gateable.
    pub const LOGIN_ANOMALY_DETECTED: &str = "login_anomaly_detected";

    /// A request presented a session cookie that resolved to a real,
    /// non-revoked session row, but one that had already crossed its idle
    /// or absolute expiry. Distinct from an ordinary 401 (missing cookie,
    /// or a token matching nothing at all) -- this specifically means a
    /// session that genuinely existed kept being used past the point it
    /// should have stopped working, which is worth a permanent row even
    /// though the response is the same opaque 401 either way.
    pub const SESSION_EXPIRED_ACCESS_ATTEMPT: &str = "session_expired_access_attempt";

    /// `tower_governor` refused a request for exceeding its bucket. Fired
    /// only for the caller-driven `TooManyRequests` case, never for the
    /// limiter's own internal-error variants -- those are a server fault,
    /// not something about the caller worth a permanent row.
    pub const RATE_LIMIT_REJECTED: &str = "rate_limit_rejected";

    /// An authenticated caller reached an admin-gated action without the
    /// role it requires. Distinct from a bare 401/403 on the wire (which
    /// stays the same either way) -- this is the durable record that lets
    /// an operator see "a real, signed-in account tried this" rather than
    /// only "someone with no session tried this", once more than one role
    /// exists to make the distinction meaningful.
    pub const AUTHORIZATION_FAILURE: &str = "authorization_failure";

    /// An administrator deactivated another user's account through the
    /// standalone disable-user action -- distinct from
    /// `ACCOUNT_RECOVERY_INITIATED`, which also passes a user through
    /// `deactivated` but as one step of reissuing an invite, not as the
    /// action's own point.
    pub const USER_DEACTIVATED: &str = "user_deactivated";

    /// An administrator changed an already-enrolled user's role through
    /// the standalone role-change action. Distinct from role being set at
    /// invite-creation time (`INVITE_CREATED`'s own metadata carries the
    /// assigned role there) -- this is a transition on an existing
    /// account, which is what `Change` is for.
    pub const ROLE_CHANGED: &str = "role_changed";

    /// An administrator reactivated a deactivated user's account, issuing a
    /// fresh invite in its place. Distinct from `ACCOUNT_RECOVERY_INITIATED`:
    /// that action starts from `active` (a lost credential on an account
    /// that could still sign in) and cycles through `deactivated` only as a
    /// mechanism to trigger the revoke-all-access-paths behaviour; this one
    /// starts from `deactivated` (access was deliberately removed) and never
    /// re-enters that state, since the revoke already happened at the
    /// original deactivation.
    pub const USER_REACTIVATED: &str = "user_reactivated";

    /// Every event type that exists, for the admin audit-log viewer's
    /// "which events" filter -- served over `GET /auth/audit-logs/event-types`
    /// rather than hand-duplicated into the frontend, so the two cannot
    /// drift the way a second, independently-maintained list would.
    ///
    /// Not derived automatically (no macro/reflection) -- adding a new
    /// event type above means adding it here too. `event_types_has_no_duplicates`
    /// below only catches a copy-paste duplicate, not an omission; there is
    /// no compiler enforcement for that half, same as this module's other
    /// deliberate choice to keep `event_type` as `text` rather than an enum.
    pub const ALL: &[&str] = &[
        LOGIN_SUCCEEDED,
        LOGIN_FAILED,
        PASSKEY_REGISTERED,
        REGISTRATION_FAILED,
        INVITE_CREATED,
        SESSION_REVOKED,
        TOTP_ENROLMENT_STARTED,
        TOTP_ENROLMENT_FAILED,
        TOTP_ENROLLED,
        INVITE_REFUSED,
        ACCOUNT_RECOVERY_INITIATED,
        TOTP_STEP_UP_SUCCEEDED,
        TOTP_STEP_UP_FAILED,
        LOGIN_ANOMALY_DETECTED,
        SESSION_EXPIRED_ACCESS_ATTEMPT,
        RATE_LIMIT_REJECTED,
        AUTHORIZATION_FAILURE,
        USER_DEACTIVATED,
        ROLE_CHANGED,
        USER_REACTIVATED,
    ];

    #[cfg(test)]
    mod tests {
        use super::ALL;
        use std::collections::HashSet;

        #[test]
        fn event_types_has_no_duplicates() {
            let unique: HashSet<&str> = ALL.iter().copied().collect();
            assert_eq!(unique.len(), ALL.len(), "ALL contains a duplicate entry");
        }
    }
}

/// What changed, for the events that are a value transition rather than a
/// bare occurrence -- see the schema's `before_state`/`after_state`
/// columns. Named and shaped like `Subjects` for the identical reason:
/// two same-typed `Option<Value>` parameters can be swapped at a call site
/// with no compiler error, and "before" and "after" transposed is exactly
/// the kind of audit-trail bug that reads as correct in review.
pub struct Change {
    pub before: Option<Value>,
    pub after: Option<Value>,
}

impl Change {
    /// Most events are occurrences, not transitions -- a login, an
    /// enrolment attempt. Nothing to diff.
    pub fn none() -> Self {
        Self {
            before: None,
            after: None,
        }
    }

    /// A genuine before/after pair -- e.g. a user's `status` column
    /// flipping from `active` to `deactivated`.
    pub fn from_to(before: Value, after: Value) -> Self {
        Self {
            before: Some(before),
            after: Some(after),
        }
    }
}

/// Who did it, and who it was done to.
///
/// These are two columns (`actor_user_id`, `target_user_id`) and they are
/// both nullable `uuid`, so passing them as bare adjacent parameters means
/// a transposition compiles cleanly and silently misattributes an
/// administrative action to the person it was performed *on*. In an audit
/// trail that is close to the worst available bug: it is wrong in exactly
/// the direction that matters and nothing about the row looks off.
///
/// Naming them at every call site removes the possibility rather than
/// documenting it away.
pub struct Subjects {
    pub actor: Option<Uuid>,
    pub target: Option<Uuid>,
}

impl Subjects {
    /// No established identity -- a login attempt against an address that
    /// may not correspond to any account, or a registration refused before
    /// anyone was resolved.
    pub fn anonymous() -> Self {
        Self {
            actor: None,
            target: None,
        }
    }

    /// Something a user did to their own account: signing in, enrolling
    /// their own passkey. `target` stays null deliberately -- repeating the
    /// same id in both columns would imply a distinction that does not
    /// exist, and would make "administrative acts" impossible to filter for
    /// later by asking for rows where the two differ.
    pub fn by(actor: Uuid) -> Self {
        Self {
            actor: Some(actor),
            target: None,
        }
    }

    /// An administrative act: `by(admin).about(subject)`.
    pub fn about(self, target: Uuid) -> Self {
        Self {
            target: Some(target),
            ..self
        }
    }
}

/// Records one audit event.
///
/// Deliberately infallible from the caller's point of view: a failure to
/// write the audit row is logged and swallowed rather than propagated.
/// Returning an error here would tempt callers into failing the request
/// they were auditing -- which would turn a logging outage into an
/// authentication outage, and hand anyone who could break audit writes a
/// denial-of-service on login. The audit trail is important, but it is
/// not more important than the operation it describes.
///
/// `subjects` carries who acted and who was acted upon -- see `Subjects`
/// for why they are named rather than positional. `ip_address` and
/// `change` were, until this signature, columns the schema had carried
/// since the very first migration with nothing ever writing them --
/// `Change::none()` and `None` are what every call site that has neither
/// available passes.
pub async fn record(
    db: &PgPool,
    event_type: &str,
    subjects: Subjects,
    user_agent: Option<&str>,
    ip_address: Option<IpNetwork>,
    change: Change,
    metadata: Value,
) {
    // No RETURNING -- see the module doc above.
    let result = sqlx::query(
        "INSERT INTO auth.auth_audit_logs
             (event_type, actor_user_id, target_user_id, user_agent, ip_address,
              before_state, after_state, metadata)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(event_type)
    .bind(subjects.actor)
    .bind(subjects.target)
    .bind(user_agent)
    .bind(ip_address)
    .bind(&change.before)
    .bind(&change.after)
    .bind(&metadata)
    .execute(db)
    .await;

    if let Err(err) = result {
        // `error`, not `warn`: a missing audit row is a real gap in a
        // record that is supposed to be complete, even though the request
        // itself is allowed to continue.
        tracing::error!(
            error = %err,
            event_type,
            actor_user_id = ?subjects.actor,
            target_user_id = ?subjects.target,
            "failed to write audit log event"
        );
    }
}
