//! A user's own ClickUp connection -- save/replace the personal API
//! token, re-test it, remove it, and read its status. Every endpoint
//! requires `integrations.clickup` (a permission an administrator grants
//! per user, see `auth_user_permissions`) and only ever touches the
//! *calling user's own* row in `integrations.user_clickup_credentials`;
//! RLS enforces that independently of this file.
//!
//! The token is validated against ClickUp before it is stored (a token
//! ClickUp rejects is never saved), encrypted at rest with
//! `integrations::secrets` under an AAD bound to the owning user, and
//! **never returned to the browser** -- the responses carry status and
//! the ClickUp identity it resolved to, nothing more.
//!
//! `status` semantics: `connected` means ClickUp accepted the token the
//! last time we checked; `invalid` means ClickUp rejected it (revoked,
//! regenerated, account deactivated). A *network* failure talking to
//! ClickUp is reported as a 502 and deliberately does NOT change the
//! stored status -- it says nothing about the token.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::AppState;
use crate::clickup::ClickUpClient;

mod credentials;
mod status;
mod token;

#[cfg(test)]
mod tests;

pub(crate) use credentials::{
    clickup_failure_response, clickup_unavailable, load_user_credentials, load_user_token,
};
pub use status::{get_connection, test_connection};
pub use token::{remove_token, save_token};

const PERMISSION: &str = "integrations.clickup";

/// A ClickUp personal token is ~40 characters; this only exists to
/// refuse an absurd paste (a whole document) before it reaches the
/// network or the database.
const MAX_TOKEN_LEN: usize = 512;

/// Bound to the owning user so a ciphertext copied from one user's row
/// into another's can never decrypt.
fn aad(user_id: Uuid) -> Vec<u8> {
    format!("user_clickup_credentials:{user_id}").into_bytes()
}

/// The production client talks to ClickUp's real API. Tests aim it at a
/// local mock through the `CLICKUP_API_BASE_URL` seam; that override is
/// compiled out of release builds so a stray environment variable can
/// never redirect users' tokens to another host.
pub(crate) fn clickup_client(state: &AppState) -> ClickUpClient {
    #[cfg(test)]
    if let Some(base_url) = state.env_source.get(crate::clickup::BASE_URL_ENV) {
        return ClickUpClient::new(&base_url);
    }

    let _ = state;
    ClickUpClient::new(crate::clickup::DEFAULT_BASE_URL)
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    /// No token saved for this user.
    NotConnected,
    /// A token is saved and ClickUp accepted it when last checked.
    Connected,
    /// A token is saved but ClickUp rejected it on the last check.
    Invalid,
}

#[derive(Debug, Serialize)]
pub struct ConnectionResponse {
    pub status: ConnectionStatus,
    pub clickup_user_id: Option<String>,
    pub clickup_username: Option<String>,
    pub last_validated_at: Option<DateTime<Utc>>,
    /// Workspace names ClickUp reported on the check this response
    /// follows. Empty on a plain status read, which makes no ClickUp
    /// call.
    pub workspace_names: Vec<String>,
}

impl ConnectionResponse {
    fn not_connected() -> Self {
        Self {
            status: ConnectionStatus::NotConnected,
            clickup_user_id: None,
            clickup_username: None,
            last_validated_at: None,
            workspace_names: Vec::new(),
        }
    }
}

#[derive(sqlx::FromRow)]
struct StatusRow {
    status: String,
    clickup_user_id: Option<String>,
    clickup_username: Option<String>,
    last_validated_at: Option<DateTime<Utc>>,
}

impl StatusRow {
    fn into_response(self, workspace_names: Vec<String>) -> ConnectionResponse {
        ConnectionResponse {
            status: if self.status == "valid" {
                ConnectionStatus::Connected
            } else {
                ConnectionStatus::Invalid
            },
            clickup_user_id: self.clickup_user_id,
            clickup_username: self.clickup_username,
            last_validated_at: self.last_validated_at,
            workspace_names,
        }
    }
}

const STATUS_COLUMNS: &str = "status, clickup_user_id, clickup_username, last_validated_at";

#[derive(Deserialize)]
pub struct SaveTokenRequest {
    pub token: String,
}

/// `Debug` never prints token: a stray `{:?}` of this
/// value in a log line must not leak a credential.
impl std::fmt::Debug for SaveTokenRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveTokenRequest")
            .field("token", &"<redacted>")
            .finish()
    }
}
