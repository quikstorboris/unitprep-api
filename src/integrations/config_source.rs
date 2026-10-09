//! Where an integration's *currently effective* configuration came from, as
//! shown on its admin settings page.

use serde::Serialize;

/// `Database` once an admin has saved the settings row; `Environment` while
/// the process is still running on `.env.local` / process env values. The
/// settings pages report it so an admin can tell a saved value from a
/// fallback that a restart or a missing row would change.
#[derive(Debug, Serialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ConfigSource {
    Database,
    Environment,
}
