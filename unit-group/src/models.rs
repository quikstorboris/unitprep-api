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
/// A discovery session can span several facilities at once — a folder
/// full of candidate unit files is normal (each facility contributes its
/// own), not necessarily duplicate re-pulls of a single facility.
/// `build_batch_from_documents` already treats every unit document as its
/// own `Facility`, aggregating distinct group names both per-facility and
/// globally — so discovery's job is to let the user confirm *which*
/// subset of candidates to process (defaulting to all of them), not to
/// force a single winner the way an earlier version of this struct did.
#[derive(Debug, Clone)]
pub struct DiscoveryResult {
    /// The unit files that will actually be validated/analyzed —
    /// always equal to `selected_unit_file_names`, exposed under its own
    /// name since that's what validate/analyze read.
    pub unit_file_names: Vec<String>,
    pub group_file_names: Vec<String>,
    pub selected_group_file_name: Option<String>,
    pub ready: bool,

    /// Every raw discovered file matching a known vendor signature — the
    /// full candidate list the user confirms a subset (or all) of.
    pub unit_file_candidates: Vec<UnitFileCandidate>,

    /// The confirmed set of unit files to process — set once the user
    /// confirms a selection, or auto-set when discovery found exactly one
    /// candidate to begin with (nothing to choose between).
    pub selected_unit_file_names: Vec<String>,

    /// `unit_file_candidates.len() > 1` and nothing confirmed yet.
    pub requires_unit_file_selection: bool,

    /// At least one file in `selected_unit_file_names` has no entry yet
    /// in `Session::format_resolutions` — the confirm-or-map step hasn't
    /// finished for all of them.
    pub requires_format_resolution: bool,

    /// Which confirmed file the confirm-or-map UI is currently working
    /// on — the first (by name) still missing a format resolution.
    /// `None` once every selected file is resolved.
    pub current_unit_file_name: Option<String>,

    /// Every confirmed file still awaiting resolution, in the order
    /// they'll be resolved — always starts with `current_unit_file_name`.
    /// Lets the UI show progress ("file 2 of 5") without re-deriving it.
    pub pending_unit_file_names: Vec<String>,

    /// The vendor detected for `current_unit_file_name`, if any.
    pub detected_vendor_name: Option<String>,

    /// `current_unit_file_name`'s own headers, exposed only while
    /// `requires_format_resolution` is true — what the manual-mapping
    /// UI's per-target dropdowns are built from.
    pub source_headers: Vec<String>,

    /// The detected vendor's preset mapping for `current_unit_file_name`,
    /// for pre-filling the manual mapping UI's dropdowns (still fully
    /// overridable). Empty when no vendor was detected.
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

    /// Distinct UnitGroup names this issue concerns — the flagged group
    /// names themselves for the two per-group checks (rare/odd groups),
    /// or the resolved UnitGroup of each affected unit for every other
    /// (per-unit) check. Lets the UI offer a group-wide fix (rename via
    /// `/correct-group`, or exclude entirely via `/exclude-group`)
    /// instead of only a per-unit one. Empty if a per-unit issue's
    /// units' UnitGroup couldn't be resolved (should not happen in
    /// practice).
    pub affected_group_names: Vec<String>,

    /// True for the two per-group checks (see `affected_group_names`) —
    /// tells the UI whether `affected_unit_ids` are real unit numbers
    /// worth listing individually, or (for these checks) just an
    /// implementation detail that happens to equal the group names.
    pub flagged_are_group_names: bool,

    /// (group name, occurrence count) pairs — populated only for "Rare
    /// UnitGroup detected", where the actual count (up to the rare-group
    /// threshold) is meaningful to show next to each name.
    pub group_occurrence_counts: Vec<(String, usize)>,
}
