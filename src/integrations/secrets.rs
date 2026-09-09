//! Encrypts a credential for storage in an integration's own settings
//! table (`client_ops.dropbox_configuration.app_secret_ciphertext`/
//! `.refresh_token_ciphertext`, `client_ops.process_street_settings.
//! api_key_ciphertext`). One key, `INTEGRATION_SECRETS_ENCRYPTION_KEY`,
//! shared across every integration's settings page -- these are all the
//! same credential class (a third-party integration's own app-wide
//! secret, configured through the admin Integrations settings pages),
//! which is exactly the case `clients::encryption`'s own module doc
//! argues *for* sharing a key: that module's per-credential-class-gets-
//! its-own-key stance is about not conflating genuinely different
//! classes (client PII vs. a user's TOTP secret vs. this), not about
//! giving every individual table its own key. `AAD` is per-integration
//! (each caller passes its own, e.g. `b"dropbox_configuration:1"`) so a
//! ciphertext can never be copied from one integration's row into
//! another's and decrypt successfully.
//!
//! Same technique as `auth::totp`/`clients::encryption`: ChaCha20-
//! Poly1305, a version-prefixed blob, AEAD additional authenticated
//! data. See either of those modules' own doc comments for the fuller
//! reasoning -- not repeated here.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};

const KEY_ENV: &str = "INTEGRATION_SECRETS_ENCRYPTION_KEY";
const FORMAT_VERSION: u8 = 1;
const NONCE_LEN: usize = 12;

fn load_key() -> Result<Key, String> {
    let hex_key =
        std::env::var(KEY_ENV).map_err(|_| format!("{KEY_ENV} is not set (see .env.local)"))?;

    let bytes = hex::decode(hex_key.trim())
        .map_err(|_| format!("{KEY_ENV} must be hex-encoded (64 characters)"))?;

    if bytes.len() != 32 {
        return Err(format!(
            "{KEY_ENV} must decode to 32 bytes, got {}",
            bytes.len()
        ));
    }

    Ok(*Key::from_slice(&bytes))
}

/// Encrypts `plaintext`, bound to `aad`. Layout: `[version:1][nonce:12]
/// [ciphertext || tag]`, random nonce per call -- this only ever runs on
/// an admin's settings save, nowhere near a volume that would make a
/// counter nonce worth the added state.
pub fn encrypt(aad: &[u8], plaintext: &str) -> Result<Vec<u8>, String> {
    let cipher = ChaCha20Poly1305::new(&load_key()?);

    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes).expect("the OS CSPRNG must be available");
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext.as_bytes(),
                aad,
            },
        )
        .map_err(|_| "failed to encrypt integration secret".to_string())?;

    let mut blob = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    blob.push(FORMAT_VERSION);
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(blob)
}

/// Reverses `encrypt`. Fails rather than returning garbage if `aad`
/// doesn't match what the blob was encrypted under.
pub fn decrypt(aad: &[u8], blob: &[u8]) -> Result<String, String> {
    if blob.len() < 1 + NONCE_LEN + 16 {
        return Err("stored integration secret is corrupt (too short)".to_string());
    }
    if blob[0] != FORMAT_VERSION {
        return Err("stored integration secret has an unrecognized format version".to_string());
    }

    let cipher = ChaCha20Poly1305::new(&load_key()?);
    let nonce = Nonce::from_slice(&blob[1..1 + NONCE_LEN]);

    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: &blob[1 + NONCE_LEN..],
                aad,
            },
        )
        // Deliberately one undifferentiated error -- same reasoning as
        // clients::encryption::decrypt / auth::totp::decrypt_secret.
        .map_err(|_| "failed to decrypt stored integration secret".to_string())?;

    String::from_utf8(plaintext)
        .map_err(|_| "decrypted integration secret is not valid UTF-8".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    fn set_test_key() {
        std::env::set_var(
            KEY_ENV,
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
    }

    fn clear_test_key() {
        std::env::remove_var(KEY_ENV);
    }

    #[test]
    #[serial(integration_secrets_encryption_key_env)]
    fn round_trips_through_encryption() {
        set_test_key();
        let blob = encrypt(b"dropbox_configuration:1", "sl.a-real-looking-refresh-token")
            .expect("encryption must succeed");
        let recovered =
            decrypt(b"dropbox_configuration:1", &blob).expect("decryption must succeed");
        assert_eq!(recovered, "sl.a-real-looking-refresh-token");
        clear_test_key();
    }

    #[test]
    #[serial(integration_secrets_encryption_key_env)]
    fn a_blob_does_not_decrypt_under_a_different_integrations_aad() {
        set_test_key();
        let blob = encrypt(b"dropbox_configuration:1", "secret").expect("encryption must succeed");
        assert!(
            decrypt(b"process_street_settings:1", &blob).is_err(),
            "a ciphertext from one integration's row must not decrypt under another's AAD"
        );
        clear_test_key();
    }

    #[test]
    #[serial(integration_secrets_encryption_key_env)]
    fn encrypting_twice_produces_different_blobs() {
        set_test_key();
        let first = encrypt(b"dropbox_configuration:1", "same-value").expect("encryption must succeed");
        let second = encrypt(b"dropbox_configuration:1", "same-value").expect("encryption must succeed");
        assert_ne!(first, second, "the nonce must be fresh per call");
        clear_test_key();
    }

    #[test]
    #[serial(integration_secrets_encryption_key_env)]
    fn refuses_to_encrypt_without_a_configured_key() {
        clear_test_key();
        assert!(encrypt(b"dropbox_configuration:1", "value").is_err());
    }

    #[test]
    #[serial(integration_secrets_encryption_key_env)]
    fn a_tampered_blob_does_not_decrypt() {
        set_test_key();
        let mut blob = encrypt(b"dropbox_configuration:1", "value").expect("encryption must succeed");
        let last = blob.len() - 1;
        blob[last] ^= 0xFF;
        assert!(decrypt(b"dropbox_configuration:1", &blob).is_err());
        clear_test_key();
    }
}
