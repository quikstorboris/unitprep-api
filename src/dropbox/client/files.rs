//! File-level Dropbox calls: download, upload (never retried) and shared
//! links.

use super::dto::DropboxError;
use super::{DropboxClient, TRANSFER_TIMEOUT};
use crate::integrations::http::{truncate_for_log, RetryPolicy, MAX_LOGGED_BODY_BYTES};

impl DropboxClient {
    // Used by api::dedup's Dropbox-import handlers
    // (download_as_uploaded_file). See dropbox::client's own #[ignore]d
    // test for list_folder coverage; this and upload below have no
    // equivalent test yet since the real network call isn't something a
    // fast unit test should exercise -- see api::dedup's own no-network
    // rejection tests for what actually is covered.
    pub async fn download(&self, path: &str) -> Result<Vec<u8>, DropboxError> {
        let response = self
            .send_authed(RetryPolicy::STANDARD, |token| {
                self.http
                    .post(self.endpoints.content_url("/2/files/download"))
                    .timeout(TRANSFER_TIMEOUT)
                    .bearer_auth(token)
                    .header("Dropbox-API-Path-Root", self.path_root_header())
                    .header(
                        "Dropbox-API-Arg",
                        serde_json::json!({ "path": path }).to_string(),
                    )
            })
            .await?;

        let status = response.status();

        if !status.is_success() {
            let body = response.text().await?;
            tracing::error!(
                path = %path,
                status = status.as_u16(),
                body = %truncate_for_log(&body, MAX_LOGGED_BODY_BYTES),
                "Dropbox download failed"
            );
            return Err(DropboxError::Api {
                status: status.as_u16(),
                body,
            });
        }

        let bytes = response.bytes().await?.to_vec();

        tracing::info!(path = %path, byte_count = bytes.len(), "dropbox download succeeded");

        Ok(bytes)
    }

    /// Uploads `bytes` to `path`, overwriting whatever is already there.
    /// Dropbox's other write modes (`add`, with conflict detection via
    /// `update`'s rev parameter) aren't exposed here -- overwrite is the
    /// only policy the one caller (api::dedup's export_to_dropbox) needs;
    /// the frontend guards against silent clobbering itself by always
    /// generating a timestamped filename, not by asking Dropbox to
    /// detect a conflict.
    pub async fn upload(&self, path: &str, bytes: Vec<u8>) -> Result<(), DropboxError> {
        let access_token = self.access_token().await?;
        let byte_count = bytes.len();

        let response = self
            .http
            .post(self.endpoints.content_url("/2/files/upload"))
            // Deliberately NOT retried (no `send_authed`): the body is
            // moved into the request, and an overwrite upload that may
            // already have landed is not something to repeat blindly.
            .timeout(TRANSFER_TIMEOUT)
            .bearer_auth(access_token)
            .header("Dropbox-API-Path-Root", self.path_root_header())
            .header(
                "Dropbox-API-Arg",
                serde_json::json!({ "path": path, "mode": "overwrite" }).to_string(),
            )
            .header("Content-Type", "application/octet-stream")
            .body(bytes)
            .send()
            .await?;

        let status = response.status();

        if !status.is_success() {
            let body = response.text().await?;
            tracing::error!(
                path = %path,
                status = status.as_u16(),
                body = %truncate_for_log(&body, MAX_LOGGED_BODY_BYTES),
                "Dropbox upload failed"
            );
            return Err(DropboxError::Api {
                status: status.as_u16(),
                body,
            });
        }

        tracing::info!(path = %path, byte_count, "dropbox upload succeeded");

        Ok(())
    }

    /// A Dropbox link to the file at `path`, reusing the one that already
    /// exists (a second request for the same file is answered by Dropbox
    /// with `shared_link_already_exists`). No audience settings are sent,
    /// so the link takes the account's own default (a team-only link in
    /// a team account). Needs the app's `sharing.write` scope; without it
    /// Dropbox answers with an error and the caller falls back to a
    /// plain web path.
    pub async fn shared_link(&self, path: &str) -> Result<String, DropboxError> {
        let reply = self
            .rpc(
                "/2/sharing/create_shared_link_with_settings",
                &serde_json::json!({ "path": path }),
                true,
            )
            .await?;

        if reply.is_success() {
            return link_url(&reply.body);
        }

        if reply.status.as_u16() == 409 && reply.body.contains("shared_link_already_exists") {
            return self.existing_shared_link(path).await;
        }

        Err(reply.into_error())
    }

    async fn existing_shared_link(&self, path: &str) -> Result<String, DropboxError> {
        let reply = self
            .rpc(
                "/2/sharing/list_shared_links",
                &serde_json::json!({ "path": path, "direct_only": true }),
                true,
            )
            .await?;

        if !reply.is_success() {
            return Err(reply.into_error());
        }

        let parsed: serde_json::Value = reply.parse("shared-link list")?;

        parsed["links"][0]["url"]
            .as_str()
            .map(str::to_string)
            .ok_or(DropboxError::Api {
                status: reply.status.as_u16(),
                body: "no shared link in the list response".to_string(),
            })
    }
}

/// The `url` of a `create_shared_link_with_settings` response.
fn link_url(body: &str) -> Result<String, DropboxError> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|parsed| parsed["url"].as_str().map(str::to_string))
        .ok_or_else(|| DropboxError::Api {
            status: 200,
            body: format!("no url in the shared-link response: {body}"),
        })
}
