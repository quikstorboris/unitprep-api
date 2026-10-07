//! HTTP layer for the QMS Template Tagging Assistant. Session-based like
//! dedup: upload+recognize happens in one step (`check`), there's no
//! separate "analyze" stage to wait for. Unlike dedup, `check` needs a DB
//! round trip first (the active `client_ops.tag_pattern` label-proximity
//! library) -- that lookup lives here, not in `TaggerSessionService`,
//! since the service has no business owning a DB connection.

mod apply;
mod dropbox;
mod files;
mod patterns;
mod recognize;
mod report;
mod views;

pub use apply::apply;
pub use dropbox::{apply_to_dropbox, save_location};
pub use recognize::{check, import_from_dropbox};
pub use report::report;

#[cfg(test)]
mod tests;
