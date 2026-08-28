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
}
