//! HTTP layer for the duplicate-tenant-check tool. Session-based, like
//! UnitGroup, but with only one real stage — see
//! `application::dedup_session_service` for why: no correction loop, no
//! in-app confirm/dismiss step, per the tool's MVP scope (list every
//! finding; corrections happen entirely outside the platform).

mod dto;
mod export;
mod export_bytes;
mod export_dropbox;
mod import_dropbox;
mod report;
mod session;
mod upload;

pub use dto::ExportFormat;
pub use export::{export, save_location};
pub(crate) use export_bytes::{file_response, generate_export};
pub use export_dropbox::export_to_dropbox;
pub use import_dropbox::import_from_dropbox;
pub use report::report;
pub use upload::check;

#[cfg(test)]
mod tests;
