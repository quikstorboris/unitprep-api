// Phase 0 only -- see process_street::mod's doc comment. Remove once
// Phase 1 gives this a real caller.
#![allow(dead_code)]

/// Everything `ProcessStreetClient` needs. A single org-wide API key --
/// unlike a per-client QMS credential, this is one secret shared across
/// every call, same shape as `dropbox::DropboxConfig`'s app-wide
/// credentials (as opposed to the encrypted-per-user-secret pattern
/// `auth::totp` uses for `TOTP_ENCRYPTION_KEY`, which doesn't apply here
/// since there is no per-user PS credential to protect).
#[derive(Clone)]
pub struct ProcessStreetConfig {
    pub api_key: String,
}

impl ProcessStreetConfig {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            api_key: std::env::var("PROCESS_STREET_API_KEY")
                .map_err(|_| "PROCESS_STREET_API_KEY is not set (see .env.local)".to_string())?,
        })
    }
}
