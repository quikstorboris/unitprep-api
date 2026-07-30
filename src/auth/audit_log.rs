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
/// for why they are named rather than positional.
pub async fn record(
    db: &PgPool,
    event_type: &str,
    subjects: Subjects,
    user_agent: Option<&str>,
    metadata: Value,
) {
    // No RETURNING -- see the module doc above.
    let result = sqlx::query(
        "INSERT INTO auth.auth_audit_logs
             (event_type, actor_user_id, target_user_id, user_agent, metadata)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(event_type)
    .bind(subjects.actor)
    .bind(subjects.target)
    .bind(user_agent)
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
