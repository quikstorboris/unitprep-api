pub mod audit_log;

mod authenticated_user;
mod authentication_ceremony;
mod ceremony_cookie;
mod registration_ceremony;
mod session_cookie;
mod session_token;
mod webauthn_backend;

pub use authenticated_user::{
    begin_owner_rls_transaction, begin_rls_transaction, try_authenticated_user, AuthenticatedUser,
    Role,
};
pub use authentication_ceremony::AuthenticationCeremony;
pub use ceremony_cookie::{
    clear_ceremony_cookie, issue_ceremony_cookie, read_ceremony_cookie, LOGIN_CEREMONY_COOKIE,
    REGISTRATION_CEREMONY_COOKIE,
};
pub use registration_ceremony::RegistrationCeremony;
pub use session_cookie::{
    clear_session_cookie, issue_session_cookie, read_session_cookie, SESSION_COOKIE_NAME,
};
pub use session_token::{generate_token, hash_token};
pub use webauthn_backend::WebauthnRsBackend;

use uuid::Uuid;

/// The JSON challenge to relay to the browser's navigator.credentials
/// call, plus opaque ceremony state that must be persisted server-side
/// (never trusted from the client) and passed back unchanged to the
/// matching finish_ method.
pub struct RegistrationChallenge {
    pub challenge: serde_json::Value,
    pub state: Vec<u8>,
}

pub struct AuthenticationChallenge {
    pub challenge: serde_json::Value,
    pub state: Vec<u8>,
}

/// A credential loaded back out of storage, for verifying an
/// authentication against. passkey_data is opaque to everything except
/// the backend that produced it -- see the schema-correction migration
/// (fix_webauthn_credentials_storage) for why this is not decomposed
/// into separate typed fields.
pub struct StoredCredential {
    pub credential_id: Vec<u8>,
    pub passkey_data: serde_json::Value,
}

/// What a successful registration ceremony produced, ready to be
/// inserted.
///
/// Deliberately a separate type from `StoredCredential` rather than
/// extra fields on it. The two are used in opposite directions -- this
/// is what the ceremony just created, `StoredCredential` is what we
/// loaded to verify against -- and only the creating direction can know
/// properties like `device_bound`. Folding them together would force
/// every load site to invent a value for a field it has no business
/// having.
pub struct RegisteredCredential {
    pub credential_id: Vec<u8>,
    pub passkey_data: serde_json::Value,

    /// True when the private key cannot leave the hardware that created
    /// it (a TPM or hardware key), false for a synced/backup-eligible
    /// passkey such as one held in iCloud Keychain or Google Password
    /// Manager.
    ///
    /// Derived from WebAuthn's Backup Eligibility (BE) flag as
    /// `!backup_eligible`: a credential the authenticator declares
    /// ineligible for backup is, by definition, one that cannot be
    /// copied off the device.
    ///
    /// Recorded for visibility only -- nothing refuses a synced passkey.
    /// See the architecture notes: requiring device-bound credentials was
    /// considered and dropped, because people legitimately work from more
    /// than one machine and Windows Hello produces a synced credential by
    /// default, so enforcing it would block the common case to defend
    /// secrets that do not exist yet.
    pub device_bound: bool,
}

/// What a successful authentication tells the caller: which stored
/// credential was used (so the caller knows which row to touch), and
/// the updated passkey_data to write back (some backends bump an
/// internal counter or other state on each use).
pub struct AuthenticationOutcome {
    pub credential_id: Vec<u8>,
    pub updated_passkey_data: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("registration ceremony failed: {0}")]
    Registration(String),

    #[error("authentication ceremony failed: {0}")]
    Authentication(String),

    #[error("ceremony state could not be read back -- it may be stale, tampered with, or from a different backend instance")]
    InvalidState,

    #[error("no credential in the provided set matched the authentication response")]
    NoMatchingCredential,
}

/// The one interface every caller (registration/login HTTP handlers)
/// depends on, rather than depending on webauthn-rs types directly --
/// per the standing interface-first design rule, a future swap (a
/// different crate, or a third-party identity service) means writing a
/// new implementation of this trait, not rewriting every call site.
///
/// Synchronous deliberately, not async: the underlying cryptographic
/// verification is CPU-bound, not I/O-bound, so there is nothing to
/// await -- matches unitprep-core's existing SessionStore trait, which
/// is synchronous for the same reason.
pub trait AuthBackend: Send + Sync {
    /// Begins passkey registration for a user. exclude carries the raw
    /// credential ids of any credentials that user already has
    /// registered, so the authenticator can refuse to create a
    /// duplicate for the same device.
    fn start_registration(
        &self,
        user_id: Uuid,
        username: &str,
        display_name: &str,
        exclude: &[Vec<u8>],
    ) -> Result<RegistrationChallenge, AuthError>;

    /// Completes registration given the browser's response (as raw JSON,
    /// exactly what the client posts) and the state returned alongside
    /// the original challenge.
    fn finish_registration(
        &self,
        response: serde_json::Value,
        state: &[u8],
    ) -> Result<RegisteredCredential, AuthError>;

    /// Begins authentication against a user's existing credentials.
    fn start_authentication(
        &self,
        credentials: &[StoredCredential],
    ) -> Result<AuthenticationChallenge, AuthError>;

    /// Completes authentication given the browser's response and the
    /// state from start_authentication, verified against the same
    /// credential set passed to start_authentication.
    fn finish_authentication(
        &self,
        response: serde_json::Value,
        state: &[u8],
        credentials: &[StoredCredential],
    ) -> Result<AuthenticationOutcome, AuthError>;
}
