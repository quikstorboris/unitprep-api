use crate::integrations::secrets;

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

/// Binds `client_ops.process_street_settings.api_key_ciphertext` to this
/// one singleton row -- see `integrations::secrets`'s own doc comment
/// and `dropbox::config::AAD`'s identical reasoning.
const AAD: &[u8] = b"process_street_settings:1";

impl ProcessStreetConfig {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            api_key: std::env::var("PROCESS_STREET_API_KEY")
                .map_err(|_| "PROCESS_STREET_API_KEY is not set (see .env.local)".to_string())?,
        })
    }

    /// Loads configuration from `client_ops.process_street_settings`,
    /// decrypting `api_key`. Returns `Ok(None)` -- not an error -- when
    /// the row has no key saved yet or the query itself fails, so the
    /// caller can fall back to `from_env()` exactly the way this
    /// integration already tolerates being unconfigured at startup. See
    /// `dropbox::config::DropboxConfig::from_db`'s identical reasoning.
    pub async fn from_db(pool: &sqlx::PgPool) -> Result<Option<Self>, String> {
        let ciphertext: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT api_key_ciphertext FROM client_ops.process_street_settings WHERE id = 1",
        )
        .fetch_optional(pool)
        .await
        .map_err(|err| err.to_string())?
        .flatten();

        let Some(ciphertext) = ciphertext else {
            return Ok(None);
        };

        Ok(Some(Self {
            api_key: secrets::decrypt(AAD, &ciphertext)?,
        }))
    }
}
