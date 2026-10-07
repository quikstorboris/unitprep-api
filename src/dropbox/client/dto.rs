//! The shapes Dropbox's JSON answers deserialize into, and the error type
//! every client call returns.

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum DropboxError {
    #[error("Dropbox request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Dropbox API returned {status}: {body}")]
    Api { status: u16, body: String },
}

/// One entry from `files/list_folder` -- deliberately minimal (just
/// enough for a future folder picker), not the full Dropbox metadata
/// shape (no size, timestamps, content hash, etc.).
#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    #[serde(rename = ".tag")]
    pub(super) tag: String,
    pub name: String,
    pub path_display: String,
}

impl Entry {
    pub fn is_folder(&self) -> bool {
        self.tag == "folder"
    }

    #[cfg(test)]
    pub(super) fn test_folder(name: &str, path_display: &str) -> Self {
        Self {
            tag: "folder".to_string(),
            name: name.to_string(),
            path_display: path_display.to_string(),
        }
    }
}

#[derive(Deserialize)]
pub(super) struct ListFolderResponse {
    pub(super) entries: Vec<Entry>,
    /// More entries exist beyond this page: fetch them with
    /// `files/list_folder/continue` and `cursor`.
    pub(super) has_more: bool,
    #[serde(default)]
    pub(super) cursor: String,
}

/// A safety valve against a cursor that never ends, not a limit anyone is
/// expected to reach: Dropbox pages hold up to 2,000 entries, so this is
/// hundreds of thousands of entries. Exceeding it is an error -- silently
/// returning a truncated listing is exactly the bug pagination fixes.
pub(super) const MAX_LIST_FOLDER_PAGES: usize = 100;

/// `files/search_v2`'s response shape is unrelated to `list_folder`'s
/// (a `matches` array of match wrappers, not a flat `entries` array),
/// but each match's inner `metadata.metadata` object has exactly the
/// same `.tag`/`name`/`path_display` fields `Entry` already parses --
/// reused as-is rather than duplicating a second near-identical struct
/// (serde ignores the extra fields search results carry, like
/// `match_type`/`highlight_spans`, since `Entry` never named them).
#[derive(Deserialize)]
pub(super) struct SearchV2Response {
    pub(super) matches: Vec<SearchV2Match>,
}

#[derive(Deserialize)]
pub(super) struct SearchV2Match {
    pub(super) metadata: SearchV2MatchMetadata,
}

#[derive(Deserialize)]
pub(super) struct SearchV2MatchMetadata {
    pub(super) metadata: Entry,
}

#[derive(Deserialize)]
pub(super) struct TokenResponse {
    pub(super) access_token: String,
    pub(super) expires_in: u64,
}

/// `sharing/get_shared_link_metadata`'s response -- deliberately just
/// `.tag`/`id`, not the fuller shape (name, link_permissions,
/// team_member_info, ...) `resolve_shared_link` doesn't need. `id` is
/// the one field worth anything here: a Dropbox-wide object identifier
/// that resolves to a real path under THIS account's own namespace via
/// `files/get_metadata`, even when this response's own path_lower would
/// be absent (see `resolve_shared_link`'s own doc comment).
#[derive(Deserialize)]
pub(super) struct SharedLinkMetadataResponse {
    #[serde(rename = ".tag")]
    pub(super) tag: String,
    pub(super) id: String,
}
