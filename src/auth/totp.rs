//! TOTP secrets: generation, encryption at rest, and code verification
//! (Phase 2 task 9).
//!
//! ## What TOTP is for here, and what it is not
//!
//! A **fallback for a device with no passkey enrolled** -- not a second step
//! stacked on a passkey. A passkey is already multi-factor (possession plus
//! biometric/PIN) and phishing-resistant because it is bound to the origin;
//! a TOTP code is neither, since it can be read off a screen and replayed by
//! an adversary-in-the-middle proxy. Requiring both would add friction to
//! every sign-in in exchange for the weaker property. Authenticator apps
//! only, never SMS.
//!
//! ## Encryption at rest, the decision this task was waiting on
//!
//! The schema named the column `secret_encrypted` and deliberately left the
//! mechanism open until something could actually write to it. Deciding now:
//! **ChaCha20-Poly1305 with a 32-byte key from `TOTP_ENCRYPTION_KEY`.**
//!
//! Why this one credential is encrypted when others are not is worth being
//! precise about, because "encrypt everything" is not the argument:
//!
//! | credential | at rest | why |
//! |---|---|---|
//! | session token | SHA-256 hash | never needs recovering; a hash suffices |
//! | invite token | SHA-256 hash | same |
//! | passkey | public key material | the secret half never leaves the authenticator |
//! | **TOTP secret** | **encrypted** | the server holds the *whole* secret and must reproduce it on every verification, so hashing is not available |
//!
//! That asymmetry is the whole reason this file exists. A TOTP secret is
//! also long-lived and not independently revocable -- there is no equivalent
//! of expiring a session -- so a database dump containing plaintext secrets
//! would hand over a working second factor for every enrolled user.
//!
//! **This is an app-level stopgap and is labelled as one.** The key sits in
//! the environment, which means it is exposed to anything that can read the
//! process environment or a `.env.local` file, and a dump *plus* the key is
//! as good as plaintext. Real KMS remains trigger-gated. What this does buy
//! is the realistic threat: a leaked database backup, a Neon branch shared
//! for debugging, a log of a query result -- none of which carry the key.
//!
//! ## Two details that are load-bearing rather than decorative
//!
//! **The ciphertext is bound to its `user_id`** via the AEAD's additional
//! authenticated data. Without that, a row's `secret_encrypted` could be
//! copied onto another user's row and would decrypt perfectly -- so anyone
//! with UPDATE on the table could graft their own known secret onto someone
//! else's account and then authenticate as them. With it, the same bytes
//! under a different `user_id` fail authentication rather than decrypting.
//!
//! **A version byte prefixes the blob.** Nothing needs it today, and that is
//! exactly when it is cheap to add: rotating the key or changing the cipher
//! later requires distinguishing old ciphertexts from new ones, and a
//! format with no version field has to be migrated blind.

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key, Nonce,
};
use totp_rs::{Algorithm, TOTP};
use uuid::Uuid;

/// Env var holding 64 hex characters (32 bytes).
const KEY_ENV: &str = "TOTP_ENCRYPTION_KEY";

/// Blob layout version. See the module docs on why this exists before it is
/// needed.
const FORMAT_VERSION: u8 = 1;

const NONCE_LEN: usize = 12;

/// 20 bytes, the SHA-1 block-appropriate length RFC 4226 recommends and what
/// every authenticator app expects. Longer is not stronger here -- HMAC-SHA1
/// keys beyond the block size are hashed down -- and shorter weakens it.
const SECRET_LEN: usize = 20;

/// Six digits over a 30-second step, the near-universal default. Anything
/// else works only with authenticators that read the parameters out of the
/// otpauth URI, and several popular ones quietly ignore them.
const DIGITS: usize = 6;
const STEP_SECONDS: u64 = 30;

/// Accept the adjacent time steps as well as the current one, so a phone
/// clock a few seconds out, or a code typed as the window turns over, still
/// works. One step each way is the usual choice: it widens the guessing
/// surface from one code to three, which is immaterial next to the
/// lockout, and removes the commonest cause of "the app says the right
/// number and it does not work".
const SKEW_STEPS: u8 = 1;

#[derive(Debug)]
pub enum TotpError {
    /// `TOTP_ENCRYPTION_KEY` is absent, malformed, or the wrong length. TOTP
    /// is unavailable; everything else in the API keeps working.
    NotConfigured(String),
    /// The stored blob could not be decrypted or authenticated -- wrong key,
    /// truncated data, an unknown version, or a ciphertext grafted from
    /// another user's row.
    Undecryptable(&'static str),
    /// The secret decrypted but is not usable as a TOTP secret.
    Unusable(String),
}

impl std::fmt::Display for TotpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TotpError::NotConfigured(detail) => {
                write!(f, "TOTP is not configured: {detail}")
            }
            TotpError::Undecryptable(detail) => write!(f, "TOTP secret unreadable: {detail}"),
            TotpError::Unusable(detail) => write!(f, "TOTP secret unusable: {detail}"),
        }
    }
}

/// Whether TOTP is available in this deployment.
///
/// Checked so the endpoints can answer "not configured" cleanly instead of
/// failing per-request in a way that reads like a bug. Deliberately *not* a
/// startup panic: the API's tool endpoints have nothing to do with TOTP, and
/// refusing to boot over an unset optional factor would take the whole
/// service down for a feature nobody may have enrolled in yet.
pub fn is_configured() -> bool {
    load_key().is_ok()
}

fn load_key() -> Result<Key, TotpError> {
    let hex_key = std::env::var(KEY_ENV)
        .map_err(|_| TotpError::NotConfigured(format!("{KEY_ENV} is not set")))?;

    let bytes = hex::decode(hex_key.trim()).map_err(|_| {
        TotpError::NotConfigured(format!("{KEY_ENV} must be hex-encoded (64 characters)"))
    })?;

    if bytes.len() != 32 {
        return Err(TotpError::NotConfigured(format!(
            "{KEY_ENV} must decode to 32 bytes, got {}",
            bytes.len()
        )));
    }

    Ok(*Key::from_slice(&bytes))
}

/// A fresh random secret, straight from the OS CSPRNG.
pub fn generate_secret() -> [u8; SECRET_LEN] {
    let mut secret = [0u8; SECRET_LEN];
    getrandom::fill(&mut secret).expect("the OS CSPRNG must be available");
    secret
}

/// Encrypts a secret for storage, bound to the user it belongs to.
///
/// Layout: `[version:1][nonce:12][ciphertext || tag]`. The nonce is random
/// per call rather than a counter -- there is no shared state to coordinate a
/// counter across processes, and a 96-bit random nonce is safe at this volume
/// (one secret per user, rewritten only on re-enrolment).
pub fn encrypt_secret(user_id: Uuid, secret: &[u8]) -> Result<Vec<u8>, TotpError> {
    let cipher = ChaCha20Poly1305::new(&load_key()?);

    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes).expect("the OS CSPRNG must be available");
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: secret,
                aad: user_id.as_bytes(),
            },
        )
        .map_err(|_| TotpError::Undecryptable("encryption failed"))?;

    let mut blob = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
    blob.push(FORMAT_VERSION);
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ciphertext);
    Ok(blob)
}

/// Reverses `encrypt_secret`. Fails rather than returning garbage if the
/// blob was written for a different user -- that is the AEAD's additional
/// data doing its job, and it is the check that makes grafting a known
/// secret onto someone else's row useless.
pub fn decrypt_secret(user_id: Uuid, blob: &[u8]) -> Result<Vec<u8>, TotpError> {
    if blob.len() < 1 + NONCE_LEN + 16 {
        return Err(TotpError::Undecryptable("blob is too short"));
    }
    if blob[0] != FORMAT_VERSION {
        return Err(TotpError::Undecryptable("unknown format version"));
    }

    let cipher = ChaCha20Poly1305::new(&load_key()?);
    let nonce = Nonce::from_slice(&blob[1..1 + NONCE_LEN]);

    cipher
        .decrypt(
            nonce,
            Payload {
                msg: &blob[1 + NONCE_LEN..],
                aad: user_id.as_bytes(),
            },
        )
        // Deliberately one undifferentiated error. Distinguishing "wrong
        // key" from "wrong user" from "tampered" would be a decryption
        // oracle, and none of the three is actionable differently by a
        // caller anyway.
        .map_err(|_| TotpError::Undecryptable("authentication failed"))
}

fn totp_for(secret: Vec<u8>, account_name: &str) -> Result<TOTP, TotpError> {
    TOTP::new(
        Algorithm::SHA1,
        DIGITS,
        SKEW_STEPS,
        STEP_SECONDS,
        secret,
        Some("UnitPrep".to_string()),
        account_name.to_string(),
    )
    .map_err(|err| TotpError::Unusable(err.to_string()))
}

/// The `otpauth://` URI an authenticator app consumes, usually by scanning a
/// QR code the frontend renders from this string.
///
/// SHA-1 is correct here and is not a weakness: RFC 6238's HMAC-SHA1
/// construction does not depend on collision resistance, and it is what
/// authenticator apps actually implement. SHA-256 TOTP exists and is
/// silently mis-handled by enough popular apps that choosing it trades real
/// interoperability for no real security.
pub fn provisioning_uri(
    user_id: Uuid,
    secret: &[u8],
    account_name: &str,
) -> Result<String, TotpError> {
    let _ = user_id;
    Ok(totp_for(secret.to_vec(), account_name)?.get_url())
}

/// The base32 secret, for typing into an app by hand when a QR code cannot
/// be scanned. Same secret as the URI carries, just the part a human copies.
pub fn base32_secret(secret: &[u8]) -> String {
    totp_rs::Secret::Raw(secret.to_vec())
        .to_encoded()
        .to_string()
}

/// Verifies a submitted code against a stored, encrypted secret.
///
/// Returns `Ok(false)` for a wrong code -- an ordinary outcome, not an
/// error. `Err` means the secret could not be read at all, which is a
/// server-side problem and must not be reported to the caller as "wrong
/// code".
pub fn verify_code(
    user_id: Uuid,
    encrypted_secret: &[u8],
    account_name: &str,
    submitted: &str,
) -> Result<bool, TotpError> {
    // Trim and strip the spaces authenticator apps display for readability
    // ("123 456"). Rejecting a code the user copied exactly as shown is a
    // support ticket, not a security control.
    let submitted: String = submitted.chars().filter(|c| !c.is_whitespace()).collect();

    if submitted.len() != DIGITS || !submitted.chars().all(|c| c.is_ascii_digit()) {
        return Ok(false);
    }

    let secret = decrypt_secret(user_id, encrypted_secret)?;
    let totp = totp_for(secret, account_name)?;

    totp.check_current(&submitted)
        .map_err(|err| TotpError::Unusable(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// A key fixed for the test process. Set rather than assumed so these do
    /// not pass or fail based on the developer's ambient environment -- the
    /// same reasoning as the old AUTH_BOOTSTRAP_ENABLED tests.
    fn with_key() {
        std::env::set_var(
            KEY_ENV,
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        );
    }

    /// RFC 6238's own test vector, which is the point: this verifies the
    /// implementation against the published standard rather than against
    /// itself. Secret is the ASCII string "12345678901234567890"; at
    /// T = 59 seconds the 6-digit SHA-1 code is 287082.
    #[test]
    fn matches_the_rfc_6238_test_vector() {
        let totp = totp_for(b"12345678901234567890".to_vec(), "rfc@example.com")
            .expect("the RFC's own secret must be usable");

        assert_eq!(totp.generate(59), "287082");
    }

    #[test]
    #[serial(totp_env)]
    fn a_secret_round_trips_through_encryption() {
        with_key();
        let user = Uuid::new_v4();
        let secret = generate_secret();

        let blob = encrypt_secret(user, &secret).expect("encryption must succeed");
        let recovered = decrypt_secret(user, &blob).expect("decryption must succeed");

        assert_eq!(recovered, secret.to_vec());
        assert_ne!(
            blob[1 + NONCE_LEN..].to_vec(),
            secret.to_vec(),
            "the stored blob must not contain the plaintext secret"
        );
    }

    /// The property that makes grafting useless: the same ciphertext under a
    /// different user_id must not decrypt. Without the AEAD's additional
    /// data this passes silently and the secret is portable between rows.
    #[test]
    #[serial(totp_env)]
    fn a_blob_does_not_decrypt_for_a_different_user() {
        with_key();
        let owner = Uuid::new_v4();
        let attacker = Uuid::new_v4();

        let blob = encrypt_secret(owner, &generate_secret()).expect("encryption must succeed");

        assert!(
            decrypt_secret(attacker, &blob).is_err(),
            "a secret encrypted for one user must not decrypt for another"
        );
    }

    #[test]
    #[serial(totp_env)]
    fn tampering_with_the_ciphertext_is_detected() {
        with_key();
        let user = Uuid::new_v4();
        let mut blob = encrypt_secret(user, &generate_secret()).expect("encryption must succeed");

        let last = blob.len() - 1;
        blob[last] ^= 0x01;

        assert!(decrypt_secret(user, &blob).is_err());
    }

    #[test]
    #[serial(totp_env)]
    fn an_unknown_format_version_is_refused() {
        with_key();
        let user = Uuid::new_v4();
        let mut blob = encrypt_secret(user, &generate_secret()).expect("encryption must succeed");

        blob[0] = 0xFF;

        assert!(decrypt_secret(user, &blob).is_err());
    }

    /// Two encryptions of the same secret must differ, or the nonce is being
    /// reused -- which for a stream cipher leaks the XOR of the plaintexts.
    #[test]
    #[serial(totp_env)]
    fn encrypting_twice_produces_different_blobs() {
        with_key();
        let user = Uuid::new_v4();
        let secret = generate_secret();

        let first = encrypt_secret(user, &secret).expect("encryption must succeed");
        let second = encrypt_secret(user, &secret).expect("encryption must succeed");

        assert_ne!(first, second, "a reused nonce would make these identical");
    }

    #[test]
    #[serial(totp_env)]
    fn a_malformed_key_is_reported_as_not_configured() {
        std::env::set_var(KEY_ENV, "not-hex");
        assert!(matches!(load_key(), Err(TotpError::NotConfigured(_))));

        std::env::set_var(KEY_ENV, "00010203");
        assert!(
            matches!(load_key(), Err(TotpError::NotConfigured(_))),
            "a key of the wrong length must be refused rather than padded"
        );

        with_key();
        assert!(load_key().is_ok());
    }

    /// Malformed submissions must be a plain "no", never an error and never
    /// a panic -- this input comes straight from a form field.
    #[test]
    #[serial(totp_env)]
    fn malformed_codes_are_rejected_without_touching_the_secret() {
        with_key();
        let user = Uuid::new_v4();
        let blob = encrypt_secret(user, &generate_secret()).expect("encryption must succeed");

        for bad in ["", "12345", "1234567", "abcdef", "12 34", "12345a"] {
            assert!(
                !verify_code(user, &blob, "user@example.com", bad).expect("must not error"),
                "expected {bad:?} to be rejected"
            );
        }
    }

    /// A code copied with the space an authenticator app displays must work.
    #[test]
    #[serial(totp_env)]
    fn whitespace_in_a_submitted_code_is_tolerated() {
        with_key();
        let user = Uuid::new_v4();
        let secret = generate_secret();
        let blob = encrypt_secret(user, &secret).expect("encryption must succeed");

        let totp = totp_for(secret.to_vec(), "user@example.com").expect("usable");
        let current = totp.generate_current().expect("the clock must be readable");
        let spaced = format!("{} {}", &current[..3], &current[3..]);

        assert!(verify_code(user, &blob, "user@example.com", &spaced).expect("must not error"));
    }

    #[test]
    fn the_provisioning_uri_names_the_issuer_and_account() {
        let secret = generate_secret();
        let uri = provisioning_uri(Uuid::new_v4(), &secret, "person@example.com")
            .expect("URI generation must succeed");

        assert!(uri.starts_with("otpauth://totp/"), "got {uri}");
        assert!(uri.contains("issuer=UnitPrep"), "got {uri}");
        assert!(uri.contains("person%40example.com"), "got {uri}");
        assert!(
            !uri.contains(&base32_secret(&secret).to_lowercase()),
            "sanity: the base32 secret is uppercase in the URI"
        );
    }
}
