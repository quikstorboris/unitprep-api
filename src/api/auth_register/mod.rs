//! WebAuthn passkey registration, HTTP side (Phase 2 task 4). The
//! cryptographic work itself lives behind `auth::AuthBackend` (see
//! `auth/mod.rs`); everything here is orchestration -- deciding *who* is
//! allowed to register, persisting the ceremony state between the two
//! requests a WebAuthn ceremony inherently needs, and writing the
//! resulting credential.
//!
//! ## Why there are two endpoints
//!
//! A WebAuthn registration is always a browser round trip: `/begin`
//! returns a challenge, page JS passes it to
//! `navigator.credentials.create()`, and `/finish` verifies whatever the
//! authenticator produced. The server-side state linking the two
//! (`PasskeyRegistration`) must never be trusted from the client, so it
//! is held server-side and referenced by a short-lived opaque cookie --
//! the same shape as the real session cookie, and for the same reason.
//!
//! ## Who is allowed to register
//!
//! Two paths, decided once in `/begin`:
//!
//! 1. **Authenticated** -- a caller with a valid session registers an
//!    additional passkey for *themselves*. The target user comes from
//!    the session, never from the request body. Also requires the
//!    session to be step-up elevated (`AuthenticatedUser::is_elevated`,
//!    see `auth_totp.rs`) -- planting a durable new credential is
//!    exactly the sensitive action step-up exists to gate, and a
//!    hijacked session cookie alone must not be sufficient for it.
//! 2. **Invite** -- the unauthenticated first-passkey path (Phase 2 task
//!    6), authorized by the token from an invitation link. Every
//!    eligibility rule is enforced inside
//!    `auth.resolve_invite_registration`: the invite must be unused and
//!    unexpired, the user must still be `invited`, and they must have
//!    **zero** existing WebAuthn credentials. Those live in the SECURITY
//!    DEFINER function rather than here deliberately -- an anonymous
//!    caller therefore cannot enumerate users (see
//!    `registration_unavailable`) and cannot enrol a competing passkey
//!    over an existing one, regardless of what this handler does.
//!
//! This replaced an env-gated `AUTH_BOOTSTRAP_ENABLED` bootstrap path,
//! which is now **deleted rather than merely unset** along with its
//! `auth.resolve_bootstrap_registration` lookup. The first administrator
//! is created by `unitprep bootstrap-admin` as an `invited` user holding
//! an invite, so they walk exactly this path too -- one enrolment route
//! exercised from the very first account, rather than a special case that
//! runs once and is therefore never really tested.
//!
//! ## When the invite is consumed, and why it is not sooner
//!
//! The invite is consumed at `/finish`, **after** the credential
//! verifies, **in the same transaction** that writes the credential. Both
//! halves of that matter, and each rules out a distinct lockout:
//!
//! * Consuming at `/begin` (or in any separate step before enrolment
//!   succeeds) would mean a cancelled authenticator prompt leaves the
//!   account `active` with no credential and a spent invite. Nothing can
//!   recover that: `bootstrap-admin --reissue-invite` deliberately
//!   refuses an account that is no longer `invited`.
//! * Consuming outside the credential transaction would leave the mirror
//!   image if either statement failed -- an `invited` user who already
//!   holds a passkey, which `--reissue-invite` also refuses (it declines
//!   any account with a credential enrolled).
//!
//! One transaction makes it all-or-nothing, so the only two reachable
//! outcomes are "enrolled and active" or "untouched and retryable".
//!
//! ## Rejections are recorded even though they are not explained
//!
//! Every refusal here returns the same opaque 403 (see
//! `registration_unavailable`) *and* writes a `registration_failed` audit row
//! naming the actual reason. Those are not in tension: the response is
//! deliberately indistinguishable so this endpoint cannot be used to
//! enumerate users, while the audit row exists so an operator can see
//! probing that the attacker believes is silent. Recording it server-side
//! leaks nothing. Before this existed, a refused registration was written
//! nowhere at all while a failed *login* wrote a row -- so the identical
//! attack was visible against one endpoint and invisible against the
//! other, which was an oversight rather than a policy.

mod begin;
mod dto;
mod enrol;
mod finish;
mod responses;
#[cfg(test)]
mod tests;

pub use begin::register_begin;
#[cfg(test)]
pub use dto::{RegisterBeginRequest, RegisterFinishRequest};
pub use finish::register_finish;

/// How long a started-but-unfinished ceremony stays valid. Deliberately
/// the same 5 minutes as the ceremony store's own timeout in `main.rs` --
/// the cookie expiring and the server-side state expiring must not
/// disagree, or one of the two silently decides the real TTL.
pub(super) const CEREMONY_TTL_MINUTES: i64 = 5;
