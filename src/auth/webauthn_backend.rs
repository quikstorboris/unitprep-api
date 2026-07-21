use uuid::Uuid;
use webauthn_rs::prelude::*;

use super::{
    AuthError,
    AuthBackend,
    AuthenticationChallenge,
    AuthenticationOutcome,
    RegistrationChallenge,
    StoredCredential,
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
        let origin = Url::parse(rp_origin).map_err(|err| {
            AuthError::Registration(format!(
                "invalid WEBAUTHN_RP_ORIGIN: {err}"
            ))
        })?;

        let webauthn = WebauthnBuilder::new(rp_id, &origin)
            .map_err(|err| {
                AuthError::Registration(format!(
                    "invalid webauthn configuration: {err}"
                ))
            })?
            .build()
            .map_err(|err| {
                AuthError::Registration(format!(
                    "invalid webauthn configuration: {err}"
                ))
            })?;

        Ok(Self { webauthn })
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
            .start_passkey_registration(
                user_id,
                username,
                display_name,
                exclude_credentials,
            )
            .map_err(|err| AuthError::Registration(err.to_string()))?;

        let challenge =
            serde_json::to_value(&challenge_response).map_err(|err| {
                AuthError::Registration(err.to_string())
            })?;

        let state = serde_json::to_vec(&reg_state).map_err(|err| {
            AuthError::Registration(err.to_string())
        })?;

        Ok(RegistrationChallenge { challenge, state })
    }

    fn finish_registration(
        &self,
        response: serde_json::Value,
        state: &[u8],
    ) -> Result<StoredCredential, AuthError> {
        let credential: RegisterPublicKeyCredential =
            serde_json::from_value(response)
                .map_err(|_| AuthError::InvalidState)?;

        let reg_state: PasskeyRegistration =
            serde_json::from_slice(state)
                .map_err(|_| AuthError::InvalidState)?;

        let passkey = self
            .webauthn
            .finish_passkey_registration(&credential, &reg_state)
            .map_err(|err| AuthError::Registration(err.to_string()))?;

        let credential_id: Vec<u8> = passkey.cred_id().as_ref().to_vec();

        let passkey_data = serde_json::to_value(&passkey).map_err(
            |err| AuthError::Registration(err.to_string()),
        )?;

        Ok(StoredCredential {
            credential_id,
            passkey_data,
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

        let challenge =
            serde_json::to_value(&challenge_response).map_err(|err| {
                AuthError::Authentication(err.to_string())
            })?;

        let state = serde_json::to_vec(&auth_state).map_err(|err| {
            AuthError::Authentication(err.to_string())
        })?;

        Ok(AuthenticationChallenge { challenge, state })
    }

    fn finish_authentication(
        &self,
        response: serde_json::Value,
        state: &[u8],
        credentials: &[StoredCredential],
    ) -> Result<AuthenticationOutcome, AuthError> {
        let credential: PublicKeyCredential =
            serde_json::from_value(response)
                .map_err(|_| AuthError::InvalidState)?;

        let auth_state: PasskeyAuthentication =
            serde_json::from_slice(state)
                .map_err(|_| AuthError::InvalidState)?;

        let result = self
            .webauthn
            .finish_passkey_authentication(&credential, &auth_state)
            .map_err(|err| AuthError::Authentication(err.to_string()))?;

        let used_credential_id: Vec<u8> =
            result.cred_id().as_ref().to_vec();

        let mut matched: Option<Passkey> = None;

        for cred in credentials {
            if cred.credential_id == used_credential_id {
                let mut passkey: Passkey =
                    serde_json::from_value(cred.passkey_data.clone())
                        .map_err(|_| AuthError::InvalidState)?;
                passkey.update_credential(&result);
                matched = Some(passkey);
                break;
            }
        }

        let passkey =
            matched.ok_or(AuthError::NoMatchingCredential)?;

        let updated_passkey_data = serde_json::to_value(&passkey)
            .map_err(|err| AuthError::Authentication(err.to_string()))?;

        Ok(AuthenticationOutcome {
            credential_id: used_credential_id,
            updated_passkey_data,
        })
    }
}
