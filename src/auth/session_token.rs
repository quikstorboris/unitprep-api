use base64::Engine;
use sha2::{Digest, Sha256};

/// Generates a new random opaque session token (256 bits of entropy)
/// and its SHA-256 hash. The raw token is what goes in the cookie and
/// is never stored anywhere; only the hash is persisted (see
/// sessions.token_hash) -- a database leak alone can never hand out a
/// usable session, since the hash cannot be reversed back into
/// something an attacker could present as a cookie.
pub fn generate_token() -> (String, Vec<u8>) {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .expect("the OS random number generator should never fail");

    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let hash = hash_token(&raw);

    (raw, hash)
}

/// Hashes a raw token the same way generate_token produces its own
/// hash -- used both when creating a session (hash before calling
/// create_session) and when verifying one (hash the cookie value
/// before calling resolve_session).
pub fn hash_token(raw: &str) -> Vec<u8> {
    Sha256::digest(raw.as_bytes()).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_token_hashes_to_the_paired_hash() {
        let (raw, hash) = generate_token();
        assert_eq!(hash_token(&raw), hash);
    }

    #[test]
    fn two_generated_tokens_never_collide() {
        let (raw_a, _) = generate_token();
        let (raw_b, _) = generate_token();
        assert_ne!(raw_a, raw_b);
    }
}
