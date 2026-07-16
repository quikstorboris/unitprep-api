use std::collections::HashMap;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct BatchRun {
    pub facilities: Vec<Facility>,
    pub global_groups: HashMap<String, usize>,
    pub advisory_issues: Vec<Issue>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Facility {
    pub name: String,
    pub source_files: Vec<String>,
    pub groups: HashMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdvisoryIssue {
    pub source: String,
    pub issue: String,
    pub severity: Severity,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl From<&str> for Severity {
    fn from(value: &str) -> Self {
        match value {
            "error" | "Error" => Severity::Error,
            "warning" | "Warning" => Severity::Warning,
            _ => Severity::Info,
        }
    }
}

pub type Issue = AdvisoryIssue;

#[derive(Debug, Clone, Serialize)]
pub struct SimilarityMatch {
    pub facility_group: String,
    pub reference_group: String,
    pub similarity: f64,
    pub difference: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AnalysisResults {
    pub batch_run: BatchRun,
    pub reference_groups: Option<Vec<String>>,
    pub net_new_groups: Vec<String>,
    pub similar_groups: Vec<SimilarityMatch>,
}

/// Brought forward from the binary's session-state type rather than
/// left behind: `analysis::reference::select_group_document` reads
/// this, and it's pure result data (no stage-machine behavior), not
/// session-envelope mechanics — the same category as `AnalysisResults`
/// above, just for an earlier pipeline stage. The session-state parts of
/// what used to be one `session.rs` (`Session`, `WorkflowStage`,
/// `StageError`) stay in the binary's `application/` layer, matching
/// `unitprep-dedup`'s own session boundary.
#[derive(Debug, Clone)]
pub struct DiscoveryResult {
    pub unit_file_names: Vec<String>,
    pub group_file_names: Vec<String>,
    pub selected_group_file_name: Option<String>,
    pub ready: bool,
}

/// Also brought forward from the binary's session-state type, same
/// reasoning as `DiscoveryResult` above — pure result data for the
/// validation stage, not stage-machine mechanics.
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub files_checked: usize,
    pub issue_count: usize,
    pub error_count: usize,
    pub warning_count: usize,
    pub issues: Vec<ValidationIssueSummary>,
    pub ready: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationIssueSummary {
    pub file_name: String,
    pub severity: Severity,
    pub description: String,
    pub affected_units: usize,
    pub affected_unit_ids: Vec<String>,
    pub detail: String,
    pub correctable_fields: Vec<String>,

    /// True only for the "Invalid dimensions" check — offers a way to
    /// mark a unit as intentionally non-dimensioned (office, apartment,
    /// etc.) instead of requiring fabricated Width/Length values.
    pub exemptable: bool,
}
