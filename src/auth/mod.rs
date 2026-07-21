mod webauthn_backend;

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

/// What gets persisted in webauthn_credentials after a successful
/// registration. passkey_data is opaque to everything except the
/// backend that produced it -- see the schema-correction migration
/// (fix_webauthn_credentials_storage) for why this is not decomposed
/// into separate typed fields.
pub struct StoredCredential {
    pub credential_id: Vec<u8>,
    pub passkey_data: serde_json::Value,
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
    ) -> Result<StoredCredential, AuthError>;

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
