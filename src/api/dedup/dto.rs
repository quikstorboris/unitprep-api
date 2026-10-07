//! Request and response shapes for the duplicate-tenant check and its exports.

use crate::api::dedup_view::DedupReportView;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize)]
pub struct DedupCheckResponse {
    pub session_id: String,
    pub report: DedupReportView,
}

#[derive(Debug, Deserialize)]
pub struct DedupSessionRequest {
    pub session_id: String,
}

/// Which file format(s) `/dedup/export` should return. Defaults to
/// `Csv` via `#[serde(default)]` on the field below, so an existing
/// caller that doesn't send this field keeps today's behavior.
#[derive(Debug, Clone, Copy, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    #[default]
    Csv,
    Xlsx,
    /// Both files in one ZIP, reusing the same `build_zip` helper
    /// Group Prep's own export already uses — one download instead of
    /// two round trips.
    Both,
}

#[derive(Debug, Deserialize)]
pub struct DedupExportRequest {
    pub session_id: String,
    #[serde(default)]
    pub format: ExportFormat,
    /// The client this check was run for, when the session was opened
    /// from a client's own Dedup tab (`/clients/{clientId}/dedup`) --
    /// `None` for a standalone run with no client context. Recorded on
    /// the Activity Log entry below so "who ran dedup for which client"
    /// is answerable without cross-referencing session ids by hand.
    #[serde(default)]
    pub client_id: Option<uuid::Uuid>,
    /// The *facility* this check was run for -- dedup runs per-facility,
    /// not per-company, so this drives the export filename's `{ABBREV}`
    /// (from `clients.facilities.name`, not the company's DBA/legal
    /// name) and per-facility version counter. Same optional/explicit
    /// shape as `client_id` above, for the same reason: the session
    /// itself carries no facility identity (see
    /// `application::dedup_session_service::DedupSession`), and `None`
    /// means a standalone run -- see `dedup_filename::standalone_file_name`.
    #[serde(default)]
    pub facility_id: Option<uuid::Uuid>,
}
