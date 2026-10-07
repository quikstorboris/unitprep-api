//! Building export files (CSV, XLSX, ZIP), their names, and the download response.

use super::dto::ExportFormat;
use crate::api::internal_error;
use crate::auth::AuthenticatedUser;
use crate::clients::dedup_filename;
use crate::infrastructure::csv_export::{build_zip, ExportFile};
use crate::infrastructure::{dedup_csv_export, dedup_xlsx_export};
use axum::response::Response;
use chrono::Utc;
use unitprep_dedup::{DedupReport, TenantRecord};

#[allow(clippy::result_large_err)]
pub(super) fn generate_csv_bytes(
    session_id: &str,
    report: &DedupReport,
    records: &[TenantRecord],
) -> Result<Vec<u8>, Response> {
    dedup_csv_export::generate_csv(report, records).map_err(|err| {
        tracing::error!(session_id = %session_id, error = %err, "Failed generating dedup export CSV");
        internal_error("Failed generating export CSV")
    })
}

#[allow(clippy::result_large_err)]
pub(super) fn generate_xlsx_bytes(
    session_id: &str,
    report: &DedupReport,
    records: &[TenantRecord],
) -> Result<Vec<u8>, Response> {
    dedup_xlsx_export::generate_xlsx(report, records).map_err(|err| {
        tracing::error!(session_id = %session_id, error = %err, "Failed generating dedup export xlsx");
        internal_error("Failed generating export xlsx")
    })
}

/// `csv_file_name`/`xlsx_file_name` are the names the two files get
/// *inside* the ZIP -- computed by the caller (`generate_export` below)
/// so this stays a pure bytes-generator with no naming logic of its own.
#[allow(clippy::result_large_err)]
pub(super) fn generate_zip_bytes(
    session_id: &str,
    report: &DedupReport,
    records: &[TenantRecord],
    csv_file_name: &str,
    xlsx_file_name: &str,
) -> Result<Vec<u8>, Response> {
    let csv_bytes = generate_csv_bytes(session_id, report, records)?;
    let xlsx_bytes = generate_xlsx_bytes(session_id, report, records)?;

    let files = vec![
        ExportFile {
            file_name: csv_file_name.to_string(),
            bytes: csv_bytes,
        },
        ExportFile {
            file_name: xlsx_file_name.to_string(),
            bytes: xlsx_bytes,
        },
    ];

    build_zip(files).map_err(|err| {
        tracing::error!(session_id = %session_id, error = %err, "Failed zipping dedup export files");
        internal_error("Failed generating export ZIP")
    })
}

/// Single format-dispatch point shared by `export()` (wraps the result in
/// an HTTP response via `file_response`) and `export_to_dropbox()` (hands
/// the bytes to `state.dropbox.upload` instead) -- the only place that
/// needs to know which generator/content-type goes with which
/// `ExportFormat`. Returns only bytes and content type now -- the actual
/// filename is a facility-scoped, DB-sequenced value (or the standalone
/// fallback) computed by `compute_export_file_names` before this is
/// called, not a static constant baked in here. `zip_inner_names` (the
/// ZIP's own two inner file names) is always passed in, even for
/// `Csv`/`Xlsx`, to keep this a plain 4-argument function rather than an
/// `Option` only one branch needs.
#[allow(clippy::result_large_err)]
pub(crate) fn generate_export(
    format: &ExportFormat,
    session_id: &str,
    report: &DedupReport,
    records: &[TenantRecord],
    zip_inner_names: (&str, &str),
) -> Result<(Vec<u8>, &'static str), Response> {
    match format {
        ExportFormat::Csv => {
            generate_csv_bytes(session_id, report, records).map(|bytes| (bytes, "text/csv"))
        }
        ExportFormat::Xlsx => generate_xlsx_bytes(session_id, report, records).map(|bytes| {
            (
                bytes,
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            )
        }),
        ExportFormat::Both => {
            let (csv_file_name, xlsx_file_name) = zip_inner_names;
            generate_zip_bytes(session_id, report, records, csv_file_name, xlsx_file_name)
                .map(|bytes| (bytes, "application/zip"))
        }
    }
}

/// The filenames one `/dedup/export`(-`dropbox`) call needs: `outer` --
/// the download/Dropbox filename, extension matching the requested
/// `ExportFormat` -- and `zip_csv`/`zip_xlsx`, the names the two files
/// get *inside* the ZIP when (and only when) `format` is `Both`. All
/// three are always populated (even when `format` isn't `Both`) so
/// `generate_export` stays a plain function with no `Option` branching --
/// they share one version number/date because computing them is exactly
/// one `increment_export_sequence` call (or one standalone-fallback
/// timestamp) reused three times, never three separate ones.
pub(super) struct ExportFileNames {
    pub(super) outer: String,
    pub(super) zip_csv: String,
    pub(super) zip_xlsx: String,
}

/// Computes the real filename(s) for one export action -- the single
/// place `export()` and `export_to_dropbox()` both go through so the
/// facility-scoped-vs-standalone decision, and the "one version number
/// per export action" rule, live in exactly one spot.
///
/// `facility_id` is `None` for a standalone run (see
/// `DedupExportRequest::facility_id`'s own doc comment) -- falls back to
/// `dedup_filename::standalone_file_name`, static for a browser download
/// (`timestamped_fallback: false`, matches today's unchanged behavior)
/// or timestamped for a Dropbox save (`timestamped_fallback: true`, the
/// anti-overwrite guarantee `useDedupSaveToDropbox.ts` used to provide
/// client-side -- see that module's own doc comment).
///
/// `Some(facility_id)` atomically increments
/// `clients.facilities.dedup_export_sequence` via
/// `dedup_filename::increment_export_sequence` -- exactly once per call,
/// regardless of `format`, since a ZIP export is one export action and
/// must consume exactly one version number for both files it bundles.
/// A DB failure here is surfaced as a 500 rather than silently falling
/// back to a wrong or duplicate filename.
pub(super) async fn compute_export_file_names(
    db: &sqlx::PgPool,
    user: &AuthenticatedUser,
    facility_id: Option<uuid::Uuid>,
    format: &ExportFormat,
    timestamped_fallback: bool,
) -> Result<ExportFileNames, Response> {
    let outer_ext = match format {
        ExportFormat::Csv => "csv",
        ExportFormat::Xlsx => "xlsx",
        ExportFormat::Both => "zip",
    };
    let now = Utc::now();

    let Some(facility_id) = facility_id else {
        return Ok(ExportFileNames {
            outer: dedup_filename::standalone_file_name(outer_ext, now, timestamped_fallback),
            zip_csv: dedup_filename::standalone_file_name("csv", now, timestamped_fallback),
            zip_xlsx: dedup_filename::standalone_file_name("xlsx", now, timestamped_fallback),
        });
    };

    let (facility_name, sequence) = dedup_filename::increment_export_sequence(
        db,
        user.user_id,
        &user.role_keys,
        facility_id,
    )
    .await
    .map_err(|err| {
        tracing::error!(error = %err, %facility_id, "Failed computing dedup export filename");
        internal_error("Failed computing export filename")
    })?;

    Ok(ExportFileNames {
        outer: dedup_filename::format_export_filename(&facility_name, sequence, outer_ext, now),
        zip_csv: dedup_filename::format_export_filename(&facility_name, sequence, "csv", now),
        zip_xlsx: dedup_filename::format_export_filename(&facility_name, sequence, "xlsx", now),
    })
}

/// What `DedupSessionService::create_session` hands back on success: the
/// new session id, the report, the ingested records and the index (into
/// the submitted files) of the file that was actually ingested.
pub(super) type CreatedDedupSession = (String, DedupReport, Vec<TenantRecord>, usize);
