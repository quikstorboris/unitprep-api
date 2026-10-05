//! The Dedup report view and export generation, run on the blocking pool.
//!
//! Building the report view assembles the whole export plan (grouping,
//! typo-variant and related-tenant sections over every tenant) and
//! generating an export writes an XLSX/CSV/ZIP in memory -- both CPU-bound,
//! both previously run straight on an async worker thread (see
//! `api::blocking` for why that matters). These wrappers give every Dedup
//! handler the same off-thread version.
//!
//! Both take the report and records **by value and hand them back**: the
//! closure handed to the blocking pool must own what it touches
//! (`'static`), and every caller still needs the data afterwards (for the
//! tool-run record, the audit event, a log line). Moving them through is
//! free; cloning them would not be.

use axum::response::Response;

use unitprep_dedup::{DedupReport, TenantRecord};

use crate::api::blocking::run_blocking;
use crate::api::dedup::{generate_export, ExportFormat};
use crate::api::dedup_view::{build_report_view, DedupReportView};

/// `build_report_view`, off the async workers. Returns the view plus the
/// inputs it was built from.
pub(crate) async fn report_view(
    report: DedupReport,
    records: Vec<TenantRecord>,
) -> Result<(DedupReportView, DedupReport, Vec<TenantRecord>), Response> {
    run_blocking("build the dedup report", move || {
        let view = build_report_view(&report, &records);
        (view, report, records)
    })
    .await
}

/// What a successful export hands back: the file bytes and their content
/// type, then the report and records the file was generated from.
pub(crate) type GeneratedExport = (Vec<u8>, &'static str, DedupReport, Vec<TenantRecord>);

/// `generate_export`, off the async workers. `Err` is a ready-to-return
/// response for either a generation failure (the handler's own error
/// response, unchanged) or a panic in the work (the standard 500).
pub(crate) async fn export(
    format: ExportFormat,
    session_id: String,
    report: DedupReport,
    records: Vec<TenantRecord>,
    zip_inner_names: (String, String),
) -> Result<GeneratedExport, Response> {
    let (generated, report, records) = run_blocking("generate the dedup export", move || {
        let generated = generate_export(
            &format,
            &session_id,
            &report,
            &records,
            (&zip_inner_names.0, &zip_inner_names.1),
        );
        (generated, report, records)
    })
    .await?;

    let (bytes, content_type) = generated?;
    Ok((bytes, content_type, report, records))
}
