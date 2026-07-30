use uuid::Uuid;
use webauthn_rs::prelude::*;

use super::{
    AuthBackend, AuthError, AuthenticationChallenge, AuthenticationOutcome, RegisteredCredential,
    RegistrationChallenge, StoredCredential,
};

/// The webauthn-rs-backed AuthBackend implementation -- the one real
/// implementation behind the trait today. See auth/mod.rs for why the
/// trait exists at all given there is currently only one of these.
pub struct WebauthnRsBackend {
    webauthn: Webauthn,
}

impl WebauthnRsBackend {
    /// rp_id must be a valid domain suffix of rp_origin (e.g. rp_id
    /// "example.com" with rp_origin "https://app.example.com") --
    /// webauthn-rs enforces this itself at build time, returning an
    /// error rather than a panic if they do not line up.
    pub fn new(rp_id: &str, rp_origin: &str) -> Result<Self, AuthError> {
        let origin = Url::parse(rp_origin)
            .map_err(|err| AuthError::Registration(format!("invalid WEBAUTHN_RP_ORIGIN: {err}")))?;

        let webauthn = WebauthnBuilder::new(rp_id, &origin)
            .map_err(|err| {
                AuthError::Registration(format!("invalid webauthn configuration: {err}"))
            })?
            .build()
            .map_err(|err| {
                AuthError::Registration(format!("invalid webauthn configuration: {err}"))
            })?;

        Ok(Self { webauthn })
    }

    /// The one place the device-bound/backup-eligible relation is written.
    ///
    /// Trivial, and named anyway: both sides are plain booleans, so a
    /// dropped `!` compiles and records the exact opposite of the truth for
    /// every credential forever. Having a single named function means the
    /// test can assert the real relation rather than a copy of it.
    ///
    /// WebAuthn's Backup Eligibility (BE) flag is set by the authenticator
    /// at creation and is static for the credential's life. A credential the
    /// authenticator declares ineligible for backup cannot be copied off the
    /// hardware that made it -- which is precisely what "device-bound"
    /// means.
    fn device_bound_from_backup_eligible(backup_eligible: bool) -> bool {
        !backup_eligible
    }

    fn deserialize_credentials(
        credentials: &[StoredCredential],
    ) -> Result<Vec<Passkey>, AuthError> {
        credentials
            .iter()
            .map(|cred| {
                serde_json::from_value(cred.passkey_data.clone())
                    .map_err(|_| AuthError::InvalidState)
            })
            .collect()
    }
}

impl AuthBackend for WebauthnRsBackend {
    fn start_registration(
        &self,
        user_id: Uuid,
        username: &str,
        display_name: &str,
        exclude: &[Vec<u8>],
    ) -> Result<RegistrationChallenge, AuthError> {
        let exclude_credentials = if exclude.is_empty() {
            None
        } else {
            Some(
                exclude
                    .iter()
                    .map(|bytes| CredentialID::from(bytes.clone()))
                    .collect(),
            )
        };

        let (challenge_response, reg_state) = self
            .webauthn
            .start_passkey_registration(user_id, username, display_name, exclude_credentials)
            .map_err(|err| AuthError::Registration(err.to_string()))?;

        let challenge = serde_json::to_value(&challenge_response)
            .map_err(|err| AuthError::Registration(err.to_string()))?;

        let state = serde_json::to_vec(&reg_state)
            .map_err(|err| AuthError::Registration(err.to_string()))?;

        Ok(RegistrationChallenge { challenge, state })
    }

    fn finish_registration(
        &self,
        response: serde_json::Value,
        state: &[u8],
    ) -> Result<RegisteredCredential, AuthError> {
        let credential: RegisterPublicKeyCredential =
            serde_json::from_value(response).map_err(|_| AuthError::InvalidState)?;

        let reg_state: PasskeyRegistration =
            serde_json::from_slice(state).map_err(|_| AuthError::InvalidState)?;

        let passkey = self
            .webauthn
            .finish_passkey_registration(&credential, &reg_state)
            .map_err(|err| AuthError::Registration(err.to_string()))?;

        let credential_id: Vec<u8> = passkey.cred_id().as_ref().to_vec();

        // `Passkey` deliberately exposes almost nothing (cred_id,
        // algorithm, public key, update_credential), so the Backup
        // Eligibility flag is not reachable from it directly. The
        // documented way through is the `From<Passkey> for Credential`
        // conversion, which yields a type whose `backup_eligible` field
        // is public -- gated behind the danger-credential-internals
        // feature, see Cargo.toml for why that is acceptable here. This is
        // a supported API rather than reaching into the serialized blob,
        // which auth/mod.rs treats as opaque on purpose and whose shape is
        // not guaranteed across versions.
        let device_bound = Self::device_bound_from_backup_eligible(
            Credential::from(passkey.clone()).backup_eligible,
        );

        let passkey_data = serde_json::to_value(&passkey)
            .map_err(|err| AuthError::Registration(err.to_string()))?;

        Ok(RegisteredCredential {
            credential_id,
            passkey_data,
            device_bound,
        })
    }

    fn start_authentication(
        &self,
        credentials: &[StoredCredential],
    ) -> Result<AuthenticationChallenge, AuthError> {
        let passkeys = Self::deserialize_credentials(credentials)?;

        let (challenge_response, auth_state) = self
            .webauthn
            .start_passkey_authentication(&passkeys)
            .map_err(|err| AuthError::Authentication(err.to_string()))?;

        let challenge = serde_json::to_value(&challenge_response)
            .map_err(|err| AuthError::Authentication(err.to_string()))?;

        let state = serde_json::to_vec(&auth_state)
            .map_err(|err| AuthError::Authentication(err.to_string()))?;

        Ok(AuthenticationChallenge { challenge, state })
    }

    fn finish_authentication(
        &self,
        response: serde_json::Value,
        state: &[u8],
        credentials: &[StoredCredential],
    ) -> Result<AuthenticationOutcome, AuthError> {
        let credential: PublicKeyCredential =
            serde_json::from_value(response).map_err(|_| AuthError::InvalidState)?;

        let auth_state: PasskeyAuthentication =
            serde_json::from_slice(state).map_err(|_| AuthError::InvalidState)?;

        let result = self
            .webauthn
            .finish_passkey_authentication(&credential, &auth_state)
            .map_err(|err| AuthError::Authentication(err.to_string()))?;

        let used_credential_id: Vec<u8> = result.cred_id().as_ref().to_vec();

        let mut matched: Option<Passkey> = None;

        for cred in credentials {
            if cred.credential_id == used_credential_id {
                let mut passkey: Passkey = serde_json::from_value(cred.passkey_data.clone())
                    .map_err(|_| AuthError::InvalidState)?;
                passkey.update_credential(&result);
                matched = Some(passkey);
                break;
            }
        }

        let passkey = matched.ok_or(AuthError::NoMatchingCredential)?;

        let updated_passkey_data = serde_json::to_value(&passkey)
            .map_err(|err| AuthError::Authentication(err.to_string()))?;

        Ok(AuthenticationOutcome {
            credential_id: used_credential_id,
            updated_passkey_data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the direction of the device_bound derivation.
    ///
    /// `device_bound` and `backup_eligible` are inverses, and both are
    /// plain booleans -- so a dropped `!` would compile, pass every other
    /// test, and silently record the exact opposite of the truth for every
    /// credential. That is the failure this file already shipped once (the
    /// column defaulted to `true` for a synced passkey), so the relation is
    /// asserted rather than left to reading comprehension.
    ///
    /// Calls the SAME function `finish_registration` uses. An earlier draft
    /// of this test re-stated the inversion locally, which proved nothing:
    /// dropping the `!` in the production path would have left it passing.
    #[test]
    fn device_bound_is_the_inverse_of_backup_eligible() {
        // A synced passkey (backup-eligible) must NOT be recorded as
        // device-bound -- the real-world case that exposed the bug.
        assert!(
            !WebauthnRsBackend::device_bound_from_backup_eligible(true),
            "a backup-eligible (synced) credential must not be marked device_bound"
        );

        // A credential that cannot be backed up is, by definition, bound to
        // the hardware that created it.
        assert!(
            WebauthnRsBackend::device_bound_from_backup_eligible(false),
            "a non-backup-eligible credential must be marked device_bound"
        );
    }
}
