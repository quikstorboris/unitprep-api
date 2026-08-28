//! Dropbox access for the QMS Onboarding folder.
//!
//! This app authorizes as a single Dropbox Business account (see
//! `config::DropboxConfig`) with Full Dropbox scope, not the narrower
//! "App folder" scope. Full Dropbox was the only option: the target
//! folder, `/QS Fileserver/Shared/QMS Onboarding`, already exists and is
//! relied on by other people day to day, so it cannot be moved into an
//! App-folder sandbox (`/Apps/<name>/`) the way that scope would require.
//!
//! The consequence is that **Dropbox itself enforces no folder boundary
//! on this token** -- it can reach anything the authorizing account can
//! see. `DropboxConfig::root_path` is an application-level convention
//! only: every caller of `DropboxClient` is expected to stay under it.
//! Nothing here stops a caller from passing a different path.
//!
//! The folder also lives in the Dropbox Business Team Space, not the
//! account's personal home namespace -- which is why every request sends
//! `Dropbox-API-Path-Root` set to `config::DropboxConfig::root_namespace_id`.
//! Omitting it would silently resolve paths against the wrong namespace
//! (the personal home) and return "not found" for a path that is
//! genuinely there.

mod client;
mod config;

// DropboxError and Entry are deliberately NOT re-exported -- nothing
// outside this module names either type yet (api::dropbox_browse maps
// over Entry's methods/fields without ever writing its name, and just
// Displays DropboxError via `%err`). Re-exporting them now would be
// exactly the "someone will need this eventually" unused import auth::
// mod.rs's own doc comment warns against -- add them back if and when a
// real caller needs to name one directly.
pub use client::DropboxClient;
pub use config::DropboxConfig;
