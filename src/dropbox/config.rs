use crate::integrations::secrets;

/// Everything `DropboxClient` needs, loaded from the five `DROPBOX_*`
/// env vars together. Grouped as one struct rather than this codebase's
/// usual one-var-one-function convention (e.g.
/// `auth::session_cookie::session_lifetime_hours`) because these five
/// are never meaningful independently -- there is no legitimate way to
/// have an app key without the matching secret, or a root path without
/// the namespace id it resolves against.
#[derive(Clone)]
pub struct DropboxConfig {
    pub app_key: String,
    pub app_secret: String,
    pub refresh_token: String,
    /// Dropbox Team Space namespace id QMS Onboarding lives under, sent
    /// as the `Dropbox-API-Path-Root` header on every request -- see
    /// this module's parent doc comment for why it's required at all.
    pub root_namespace_id: String,
    /// App-level convention, not a Dropbox-enforced boundary -- see the
    /// parent module doc comment.
    pub root_path: String,
}

/// Binds every `client_ops.dropbox_configuration` ciphertext to this one
/// singleton row -- see `integrations::secrets`'s own doc comment on why
/// AAD is per-integration, not per-key. A fixed value is correct here
/// (unlike `clients::encryption`'s per-row AAD, which binds to a
/// variable facility/party) because this table only ever has the one
/// row, `id = 1`.
const AAD: &[u8] = b"dropbox_configuration:1";

impl DropboxConfig {
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| {
            std::env::var(name).map_err(|_| format!("{name} is not set (see .env.local)"))
        };

        Ok(Self {
            app_key: var("DROPBOX_APP_KEY")?,
            app_secret: var("DROPBOX_APP_SECRET")?,
            refresh_token: var("DROPBOX_REFRESH_TOKEN")?,
            root_namespace_id: var("DROPBOX_ROOT_NAMESPACE_ID")?,
            root_path: var("DROPBOX_ROOT_PATH")?,
        })
    }

    /// Loads configuration from `client_ops.dropbox_configuration`,
    /// decrypting `app_secret`/`refresh_token`. Returns `Ok(None)` --
    /// not an error -- when the row is missing required fields (not yet
    /// filled in via the admin settings page) or the query itself fails
    /// (e.g. this migration hasn't been applied yet in this
    /// environment), so the caller can fall back to `from_env()` exactly
    /// the way every deployment did before this table existed. A caller
    /// that wants that fallback distinguished from a genuine decryption
    /// failure should check the `Err` case separately -- this only
    /// collapses "no row"/"incomplete row"/"query failed" into `None`.
    pub async fn from_db(pool: &sqlx::PgPool) -> Result<Option<Self>, String> {
        #[allow(clippy::type_complexity)]
        let row: Option<(
            Option<String>,
            Option<Vec<u8>>,
            Option<Vec<u8>>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT app_key, app_secret_ciphertext, refresh_token_ciphertext, root_namespace_id, root_path
               FROM client_ops.dropbox_configuration WHERE id = 1",
        )
        .fetch_optional(pool)
        .await
        .map_err(|err| err.to_string())?;

        let Some((
            Some(app_key),
            Some(app_secret_ciphertext),
            Some(refresh_token_ciphertext),
            Some(root_namespace_id),
            Some(root_path),
        )) = row
        else {
            return Ok(None);
        };

        Ok(Some(Self {
            app_key,
            app_secret: secrets::decrypt(AAD, &app_secret_ciphertext)?,
            refresh_token: secrets::decrypt(AAD, &refresh_token_ciphertext)?,
            root_namespace_id,
            root_path,
        }))
    }
}
