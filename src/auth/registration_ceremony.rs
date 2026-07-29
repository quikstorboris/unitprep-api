use uuid::Uuid;

use unitprep_core::session::{HasSessionMetadata, SessionMetadata};

/// Ephemeral state for one in-progress WebAuthn registration ceremony --
/// the gap between POST /auth/register/begin (issues a challenge) and
/// POST /auth/register/finish (verifies the browser's response against
/// it). Stored the same way unit-group/dedup sessions are (see
/// unitprep_core::in_memory_session_store), not in a database table --
/// this state is meaningless once the ceremony completes or expires,
/// and unlike a real session it must never survive a process restart or
/// be reachable by anything other than the exact ceremony that created
/// it.
pub struct RegistrationCeremony {
    pub metadata: SessionMetadata,
    pub user_id: Uuid,

    /// webauthn-rs's own serialized PasskeyRegistration state, opaque to
    /// everything except AuthBackend::finish_registration -- see
    /// RegistrationChallenge in auth/mod.rs.
    pub webauthn_state: Vec<u8>,

    /// True when this ceremony was started through the unauthenticated
    /// bootstrap path rather than by an already-signed-in user adding an
    /// additional passkey. Decided at `begin` and carried here rather
    /// than re-derived at `finish`: whether a session cookie happens to
    /// be present on the *second* request is a different question from
    /// which path this ceremony was authorized under, and only the
    /// bootstrap case should end with a newly issued session (an
    /// already-authenticated caller keeps the session they arrived with).
    pub is_bootstrap: bool,
}

impl RegistrationCeremony {
    pub fn new(id: String, user_id: Uuid, webauthn_state: Vec<u8>, is_bootstrap: bool) -> Self {
        Self {
            metadata: SessionMetadata::new(id, Some(user_id)),
            user_id,
            webauthn_state,
            is_bootstrap,
        }
    }
}

impl HasSessionMetadata for RegistrationCeremony {
    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut SessionMetadata {
        &mut self.metadata
    }
}
