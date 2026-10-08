//! Cross-cutting support for the admin "Integrations" settings pages
//! (Process Street, Dropbox, and whatever comes next -- ClickUp/Claude
//! are named as follow-ups in the vault's own design note). Two concerns
//! every integration's settings page shares, factored out here rather
//! than copy-pasted per integration:
//!
//! - [`config_source`]: the `Database`/`Environment` marker a settings page
//!   reports for each value.
//! - [`secrets`]: encrypting a credential for storage in that
//!   integration's own settings table.
//! - [`http`]: the shared outbound-HTTP policy (timeouts, retries, log-safe
//!   body truncation) every integration client is built on.
//! - [`env_source`]: reading a credential's *currently effective* value
//!   when no row has been saved yet, so a settings page can show what's
//!   actually running rather than a blank form -- and so that lookup has
//!   one seam to change if `.env.local`/process env ever stops being
//!   where these live (a secrets manager, a hosting provider's own
//!   config API, etc.).

pub mod config_source;
pub mod env_source;
pub mod http;
pub mod secrets;
