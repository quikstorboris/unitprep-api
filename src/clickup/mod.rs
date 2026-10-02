//! ClickUp integration -- the connection check (does this personal API
//! token work, and who does it belong to), the onboarding hierarchy
//! (folders/lists), fuzzy matching of Orchestrator facilities to ClickUp
//! lists, and parsing of pasted ClickUp links. Task actions (status,
//! due date, comment, create under a phase) will build on `client`.
//!
//! Unlike Dropbox/Process Street, there is no app-wide credential: each
//! Orchestrator user connects their own ClickUp personal API token
//! (`integrations.user_clickup_credentials`, see `api::clickup_connection`)
//! and every call is made with the acting user's own token, so ClickUp's
//! own activity history attributes work to the person who did it. The
//! client itself is therefore stateless and token-per-call.
//!
//! Design record: the vault's `work/active/UnitPrep/ClickUp Integration/`
//! design log.

pub mod assignment;
mod client;
pub mod hierarchy;
pub mod matching;
pub mod url;

#[cfg(test)]
pub use client::BASE_URL_ENV;
pub use client::{ClickUpClient, ClickUpError, ClickUpIdentity, DEFAULT_BASE_URL};
