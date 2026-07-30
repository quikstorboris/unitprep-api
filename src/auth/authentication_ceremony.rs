use uuid::Uuid;

use unitprep_core::session::{HasSessionMetadata, SessionMetadata};

/// Ephemeral state for one in-progress WebAuthn *login* ceremony -- the
/// gap between POST /auth/login/begin (issues a challenge) and POST
/// /auth/login/finish (verifies the authenticator's response against it).
/// Mirrors RegistrationCeremony, and stored the same way: through
/// unitprep_core's generic SessionStore rather than a table, because the
/// state is meaningless once the ceremony completes or expires and must
/// not survive a process restart.
///
/// Only the user_id is carried, not the credential set. The credentials
/// are re-read from the database at `finish` instead, for two reasons:
///
///   1. This id is server-side and was never client-supplied, so `finish`
///      can set app.current_user_id from it and read the same rows through
///      normal owner-scoped RLS -- no SECURITY DEFINER bypass needed on
///      that leg, unlike `begin` which starts from an email alone.
///   2. A credential set copied in here at `begin` could go stale within
///      the ceremony's lifetime (the user revokes a passkey in another
///      tab), and verifying against a stale copy is exactly the kind of
///      check that appears to work while having stopped meaning anything.
pub struct AuthenticationCeremony {
    pub metadata: SessionMetadata,
    pub user_id: Uuid,

    /// Non-secret id for correlating this ceremony's two halves in the
    /// logs -- same reasoning as `RegistrationCeremony::correlation_id`,
    /// and deliberately not `metadata.id`, which is the ceremony cookie's
    /// contents.
    pub correlation_id: Uuid,

    /// webauthn-rs's own serialized PasskeyAuthentication state, opaque to
    /// everything except AuthBackend::finish_authentication -- see
    /// AuthenticationChallenge in auth/mod.rs.
    pub webauthn_state: Vec<u8>,
}

impl AuthenticationCeremony {
    pub fn new(id: String, user_id: Uuid, webauthn_state: Vec<u8>) -> Self {
        Self {
            metadata: SessionMetadata::new(id, Some(user_id)),
            user_id,
            correlation_id: Uuid::new_v4(),
            webauthn_state,
        }
    }
}

impl HasSessionMetadata for AuthenticationCeremony {
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

    /// Same guard as the registration ceremony's: the id that gets logged
    /// must not be the id that gets put in a cookie.
    #[test]
    fn the_correlation_id_is_not_the_ceremony_id() {
        let ceremony = AuthenticationCeremony::new(
            "login-cookie-value".to_string(),
            Uuid::new_v4(),
            Vec::new(),
        );

        assert_ne!(
            ceremony.correlation_id.to_string(),
            ceremony.metadata.id,
            "the logged correlation id must never be the ceremony cookie's value"
        );
    }

    #[test]
    fn two_ceremonies_for_one_user_get_different_correlation_ids() {
        let user_id = Uuid::new_v4();

        let first = AuthenticationCeremony::new("a".to_string(), user_id, Vec::new());
        let second = AuthenticationCeremony::new("b".to_string(), user_id, Vec::new());

        assert_ne!(first.correlation_id, second.correlation_id);
    }
}
