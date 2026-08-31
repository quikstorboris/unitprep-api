//! Field-level encryption for the genuinely sensitive values Process
//! Street's New Merchant Account workflow captures: SSN, date of birth,
//! and home address for the signer and every listed owner, plus bank
//! routing/account numbers, EIN, and the QMS/processor system
//! credentials (`QUIKSTOR_Password`, `QSS_API_Pin`, `QSS_WEB_PIN`,
//! `Pinpad_User_ID`) the same form hands over.
//!
//! This is the trigger the vault's 2026-08-07 PII/compliance review
//! named in advance: "SSN/EIN/TIN handling for a possible future Elavon
//! -application-prefill tool ... the first genuinely sensitive
//! -government-ID use case identified for this platform, and it is the
//! trigger that would justify real field-level encryption." That review
//! deliberately rejected near-term encryption for everything else
//! (name/address/phone/email is the accepted tier) -- this module is
//! the "except" clause, not a reversal of that call.
//!
//! Same technique as `auth::totp` (ChaCha20-Poly1305, a version-prefixed
//! blob, AEAD additional authenticated data binding the ciphertext to
//! the row it belongs to) under its own key and its own module, not a
//! shared abstraction with TOTP -- this is a different credential class
//! bound to a different key (a facility, or a specific party within a
//! facility, rather than a user), and TOTP's own module doc argues for
//! exactly this kind of small, explicit, per-purpose module rather than
//! a generalized "encrypt anything" helper. Read `auth::totp`'s module
//! doc first if this comment doesn't fully explain a design choice --
//! most of the reasoning here is inherited from there verbatim.
//!
//! **Callers must never log a decrypted value.** Nothing in this module
//! logs plaintext, but that guarantee only holds if every call site
//! also avoids it -- `tracing::info!`/`error!` on a decrypted SSN or
//! password would defeat the entire point.

// Phase 1 only -- nothing in the rest of the crate calls into this yet
// (no HTTP handler ingests a Merchant Account run). Remove once a real
// caller exists.
#![allow(dead_code)]

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};

/// Env var holding 64 hex characters (32 bytes). Deliberately a
/// separate key from `TOTP_ENCRYPTION_KEY` -- different credential
/// class, different blast radius if one key ever leaks.
const KEY_ENV: &str = "CLIENT_PII_ENCRYPTION_KEY";

/// Blob layout version. See `auth::totp`'s module doc on why this
/// exists before it's needed: rotating the key or changing the cipher
/// later requires distinguishing old ciphertexts from new ones.
const FORMAT_VERSION: u8 = 1;

const NONCE_LEN: usize = 12;

#[derive(Debug)]
pub enum EncryptionError {
    /// `CLIENT_PII_ENCRYPTION_KEY` is absent, malformed, or the wrong
    /// length.
    NotConfigured(String),
    /// The stored blob could not be decrypted or authenticated -- wrong
    /// key, truncated data, an unknown version, or a ciphertext grafted
    /// from a different row's AAD.
    Undecryptable(&'static str),
}

impl std::fmt::Display for EncryptionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncryptionError::NotConfigured(detail) => {
                write!(f, "client PII encryption is not configured: {detail}")
            }
            EncryptionError::Undecryptable(detail) => {
                write!(f, "encrypted client PII unreadable: {detail}")
            }
        }
    }
}

impl std::error::Error for EncryptionError {}

/// Whether encryption is available in this deployment. Checked so
/// ingestion can fail loudly and early (refuse to ingest a Merchant
/// Account run at all) rather than silently skipping the sensitive
/// fields or, worse, falling back to storing them unencrypted.
pub fn is_configured() -> bool {
    load_key().is_ok()
}

fn load_key() -> Result<Key, EncryptionError> {
    let hex_key = std::env::var(KEY_ENV)
        .map_err(|_| EncryptionError::NotConfigured(format!("{KEY_ENV} is not set")))?;

    let bytes = hex::decode(hex_key.trim()).map_err(|_| {
        EncryptionError::NotConfigured(format!("{KEY_ENV} must be hex-encoded (64 characters)"))
    })?;

    if bytes.len() != 32 {
        return Err(EncryptionError::NotConfigured(format!(
            "{KEY_ENV} must decode to 32 bytes, got {}",
            bytes.len()
        )));
    }

    Ok(*Key::from_slice(&bytes))
}

/// Encrypts `plaintext`, bound to `aad` (additional authenticated
/// data). Callers choose `aad` to identify the specific row/record this
/// ciphertext belongs to -- e.g. a facility's id for a facility-level
/// secrets bundle, or `facility_id:role:index` for one party's PII --
/// so the same bytes decrypted under a different `aad` fail rather
/// than succeeding. Without this, a ciphertext could be copied onto a
/// different row (or a different party within the same facility) and
/// would decrypt perfectly.
///
/// Layout: `[version:1][nonce:12][ciphertext || tag]`. The nonce is
/// random per call, not a counter -- there is no shared state to
/// coordinate a counter across processes, and a 96-bit random nonce is
/// safe at this volume (a handful of writes per facility, not a
/// high-frequency stream).
pub fn encrypt(aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, EncryptionError> {
    let cipher = ChaCha20Poly1305::new(&load_key()?);

    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes).expect("the OS CSPRNG must be available");
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, Payload { msg: plaintext, aad })
        .map_err(|_| EncryptionError::Undecryptable("encryption failed"))?;

    let mut blob = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    blob.push(FORMAT_VERSION);
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(blob)
}

/// Reverses `encrypt`. Fails rather than returning garbage if `aad`
/// doesn't match what the blob was encrypted under.
pub fn decrypt(aad: &[u8], blob: &[u8]) -> Result<Vec<u8>, EncryptionError> {
    if blob.len() < 1 + NONCE_LEN + 16 {
        return Err(EncryptionError::Undecryptable("blob is too short"));
    }
    if blob[0] != FORMAT_VERSION {
        return Err(EncryptionError::Undecryptable("unknown format version"));
    }

    let cipher = ChaCha20Poly1305::new(&load_key()?);
    let nonce = Nonce::from_slice(&blob[1..1 + NONCE_LEN]);

    cipher
        .decrypt(
            nonce,
            Payload {
                msg: &blob[1 + NONCE_LEN..],
                aad,
            },
        )
        // Deliberately one undifferentiated error -- same reasoning as
        // auth::totp::decrypt_secret: distinguishing "wrong key" from
        // "wrong aad" from "tampered" would be a decryption oracle, and
        // none of the three is actionable differently by a caller.
        .map_err(|_| EncryptionError::Undecryptable("authentication failed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    // Mirrors auth::totp's own test env-var handling: several tests
    // here set/remove the process-global CLIENT_PII_ENCRYPTION_KEY,
    // which races under the default parallel test runner. #[serial]
    // gives them a shared named lock, same technique already used for
    // TOTP_ENCRYPTION_KEY tests.
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
    #[serial(client_pii_encryption_key_env)]
    fn round_trips_through_encryption() {
        set_test_key();
        let plaintext = b"394-56-7868";
        let blob = encrypt(b"facility-123:owner:1", plaintext).expect("encryption must succeed");
        let recovered = decrypt(b"facility-123:owner:1", &blob).expect("decryption must succeed");
        assert_eq!(recovered, plaintext);
        clear_test_key();
    }

    #[test]
    #[serial(client_pii_encryption_key_env)]
    fn a_blob_does_not_decrypt_under_different_aad() {
        set_test_key();
        let blob = encrypt(b"facility-123:owner:1", b"secret").expect("encryption must succeed");
        assert!(
            decrypt(b"facility-123:owner:2", &blob).is_err(),
            "a blob encrypted for one party must not decrypt for a different party, \
             even within the same facility"
        );
        clear_test_key();
    }

    #[test]
    #[serial(client_pii_encryption_key_env)]
    fn a_blob_does_not_decrypt_after_tampering() {
        set_test_key();
        let mut blob = encrypt(b"facility-123", b"secret").expect("encryption must succeed");
        let last = blob.len() - 1;
        blob[last] ^= 0xFF;
        assert!(decrypt(b"facility-123", &blob).is_err());
        clear_test_key();
    }

    #[test]
    #[serial(client_pii_encryption_key_env)]
    fn encrypting_twice_produces_different_blobs() {
        set_test_key();
        let first = encrypt(b"facility-123", b"secret").expect("encryption must succeed");
        let second = encrypt(b"facility-123", b"secret").expect("encryption must succeed");
        assert_ne!(first, second, "the nonce must be fresh per call");
        clear_test_key();
    }

    #[test]
    #[serial(client_pii_encryption_key_env)]
    fn refuses_to_encrypt_without_a_configured_key() {
        clear_test_key();
        assert!(!is_configured());
        assert!(matches!(
            encrypt(b"facility-123", b"secret"),
            Err(EncryptionError::NotConfigured(_))
        ));
    }

    #[test]
    #[serial(client_pii_encryption_key_env)]
    fn rejects_a_key_of_the_wrong_length() {
        std::env::set_var(KEY_ENV, "AABB");
        assert!(matches!(
            encrypt(b"facility-123", b"secret"),
            Err(EncryptionError::NotConfigured(_))
        ));
        clear_test_key();
    }
}
