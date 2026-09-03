//! Renders the admin audit-log export as a PDF, entirely in memory.
//!
//! Uses `printpdf`'s built-in Helvetica/Helvetica-Bold fonts (one of the
//! 14 standard PDF fonts every viewer already has) rather than embedding a
//! TTF file -- zero extra assets to ship with the binary, same
//! self-hosted-library-with-minimal-footprint preference as the rest of
//! this project. `printpdf` itself is pulled in with `default-features =
//! false` (see Cargo.toml) specifically to avoid its default HTML/CSS
//! layout engine (`azul`), which this module has no use for: a fixed-
//! column table with manual pagination needs none of that.
//!
//! Column positions/widths and per-cell character-count limits (see
//! `layout`) are a rough heuristic (Helvetica's average glyph width at the
//! chosen font size), not a text-measuring layout engine -- good enough
//! for an internal admin report, where a slightly conservative truncation
//! is a non-issue and a real layout engine would be a lot of dependency
//! for very little benefit.

mod layout;
mod render;

pub use render::render_audit_log_pdf;

pub struct AuditLogPdfRow {
    pub created_at: String,
    pub event_type: String,
    pub actor_label: String,
    pub target_label: String,
    pub ip_address: String,
    pub details: String,
}

impl AuditLogPdfRow {
    /// Every column except Details, which wraps onto multiple lines and
    /// so isn't rendered through the single-line generic path the rest
    /// of the row uses.
    fn single_line_cells(&self) -> [&str; 5] {
        [
            &self.created_at,
            &self.event_type,
            &self.actor_label,
            &self.target_label,
            &self.ip_address,
        ]
    }
}

pub struct AuditLogPdfReport {
    /// e.g. "UnitPrep Security Log Report" / "UnitPrep Activity Log
    /// Report" -- the only thing that differs between the security and
    /// activity exports, which otherwise share this entire renderer.
    pub report_title: String,
    pub generated_by: String,
    pub generated_at: String,
    /// Pre-formatted lines, one per filter -- kept as plain strings rather
    /// than a structured filter type, since this module's only job is
    /// laying text on a page, not deciding how a filter should read.
    pub filter_lines: Vec<String>,
    pub rows: Vec<AuditLogPdfRow>,
    /// Set when the result set was capped -- (rows shown, the cap itself),
    /// rendered as a closing note rather than silently returning a partial
    /// export with no indication anything was left out.
    pub truncated_at: Option<usize>,
}
