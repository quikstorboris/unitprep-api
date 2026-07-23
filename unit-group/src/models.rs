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

/// One (target field, source header) pairing shown to the user as a
/// pre-fill suggestion in the manual-mapping UI — always fully resolved
/// (both sides present), unlike the mapping the user actually submits,
/// where a target can be left unmapped.
#[derive(Debug, Clone, Serialize)]
pub struct FieldMappingEntry {
    pub target: String,
    pub source: String,
}

/// A single discovered file that matched a known vendor's header
/// signature (see `crate::format::detect_vendor`) — a candidate to become
/// the session's one selected unit file. Carries its modified-at time
/// (when the browser sent one) specifically so the UI can help a user
/// pick the right file when a folder contains more than one candidate,
/// e.g. several dated re-pulls of the same facility's export.
#[derive(Debug, Clone, Serialize)]
pub struct UnitFileCandidate {
    pub file_name: String,
    pub modified_at: Option<i64>,
    pub detected_vendor: String,
}

/// Brought forward from the binary's session-state type rather than
/// left behind: `analysis::reference::select_group_document` reads
/// this, and it's pure result data (no stage-machine behavior), not
/// session-envelope mechanics — the same category as `AnalysisResults`
/// above, just for an earlier pipeline stage. The session-state parts of
/// what used to be one `session.rs` (`Session`, `WorkflowStage`,
/// `StageError`) stay in the binary's `application/` layer, matching
/// `unitprep-dedup`'s own session boundary.
///
/// A discovery session is scoped to one facility: any time more than one
/// candidate unit file is found, that's always redundant/duplicate pulls
/// of that one facility (e.g. repeated exports on different dates), never
/// intentionally-distinct facilities — so exactly one must be selected
/// before anything downstream (format resolution, validation, analysis)
/// can run against it.
#[derive(Debug, Clone)]
pub struct DiscoveryResult {
    /// At most one entry once resolved — kept as a `Vec` for backward
    /// display compatibility with the "unit files found" count; derived
    /// from `unit_file_candidates`/`selected_unit_file_name` rather than
    /// tracked independently.
    pub unit_file_names: Vec<String>,
    pub group_file_names: Vec<String>,
    pub selected_group_file_name: Option<String>,
    pub ready: bool,

    /// Every raw discovered file matching a known vendor signature.
    pub unit_file_candidates: Vec<UnitFileCandidate>,

    /// Set once the user picks one (or discovery found exactly one to
    /// begin with).
    pub selected_unit_file_name: Option<String>,

    /// `unit_file_candidates.len() > 1` and nothing selected yet.
    pub requires_unit_file_selection: bool,

    /// Exactly one unit file selected, but it has no entry yet in
    /// `Session::format_resolutions` — the confirm-or-map step hasn't run.
    pub requires_format_resolution: bool,

    /// The vendor detected for the selected file, if any.
    pub detected_vendor_name: Option<String>,

    /// The selected file's own headers, exposed only while
    /// `requires_format_resolution` is true — what the manual-mapping
    /// UI's per-target dropdowns are built from.
    pub source_headers: Vec<String>,

    /// The detected vendor's preset mapping, for pre-filling the manual
    /// mapping UI's dropdowns (still fully overridable). Empty when no
    /// vendor was detected for the selected file.
    pub suggested_mapping: Vec<FieldMappingEntry>,
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
    pub files_errored: Vec<FileValidationError>,
    pub ready: bool,
}

/// One discovered unit file that couldn't be validated at all, due to
/// an internal inconsistency (see `validate_document`'s `Err` path) —
/// distinct from a `ValidationIssueSummary`, which describes a real
/// data-quality problem *found* in a file that was otherwise
/// successfully checked. This should never look like a clean/absent
/// result: a file landing here means validation never actually ran on
/// it, which `ready` must reflect (see `run_validation`).
#[derive(Debug, Clone, Serialize)]
pub struct FileValidationError {
    pub file_name: String,
    pub message: String,
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
