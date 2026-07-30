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

    /// Non-secret id for correlating this ceremony's two halves in the
    /// logs. Deliberately NOT `metadata.id`: that value is the ceremony
    /// cookie's contents, so logging it would put a live bearer value into
    /// ops output that is routinely shipped somewhere less protected than
    /// the database. This one is generated alongside it, never leaves the
    /// server, and is safe to log -- which is the whole point, since two
    /// concurrent ceremonies for the same user are otherwise
    /// indistinguishable in the log.
    pub correlation_id: Uuid,

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
            correlation_id: Uuid::new_v4(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason `correlation_id` exists at all: the log-safe id must not
    /// be the cookie's value. A later "simplification" that logged
    /// `metadata.id` instead, or assigned it here, would put a live bearer
    /// value into ops output -- and would look like a tidy-up in review.
    #[test]
    fn the_correlation_id_is_not_the_ceremony_id() {
        let ceremony = RegistrationCeremony::new(
            "ceremony-cookie-value".to_string(),
            Uuid::new_v4(),
            Vec::new(),
            false,
        );

        assert_ne!(
            ceremony.correlation_id.to_string(),
            ceremony.metadata.id,
            "the logged correlation id must never be the ceremony cookie's value"
        );
    }

    /// Two ceremonies for the SAME user must be tellable apart, which is
    /// the property that makes the id worth logging -- two browser tabs
    /// enrolling at once are indistinguishable by `user_id` alone.
    #[test]
    fn two_ceremonies_for_one_user_get_different_correlation_ids() {
        let user_id = Uuid::new_v4();

        let first = RegistrationCeremony::new("a".to_string(), user_id, Vec::new(), false);
        let second = RegistrationCeremony::new("b".to_string(), user_id, Vec::new(), false);

        assert_ne!(first.correlation_id, second.correlation_id);
    }
}
