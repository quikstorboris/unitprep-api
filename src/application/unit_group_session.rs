//! Group Prep's session envelope and stage machine — the one piece of
//! session state that stays in the binary rather than moving to
//! `unitprep-unit-group`, matching the boundary `unitprep-dedup`
//! already established: pure logic/result data lives in the domain
//! crate, session orchestration lives here. `DiscoveryResult`,
//! `ValidationResult`, and `ValidationIssueSummary` moved to the crate
//! (pure result data, not stage-machine behavior); `Session`,
//! `WorkflowStage`, `StageError`, and `SessionData` — the actual stage
//! machine — stay here.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use uuid::Uuid;

use unitprep_core::csv_document::CsvDocument;
use unitprep_core::session::{HasSessionMetadata, SessionMetadata};
use unitprep_unit_group::{
    apply_corrections, apply_field_mapping, detect_vendor, filter_excluded_groups,
    mapping_from_vendor, AnalysisResults, CorrectionKey, DimensionExemptionKey, DiscoveryResult,
    FieldMapping, GroupCheckAcknowledgmentKey, ValidationResult,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkflowStage {
    Uploaded,
    Discovered,
    Validated,
    Analyzed,
    Exported,
}

#[derive(Debug, Clone, Default)]
pub struct SessionData {
    pub documents: Arc<Vec<CsvDocument>>,
    pub discovery: Option<DiscoveryResult>,
    pub validation: Option<ValidationResult>,
    // Arc, not an owned value -- mirrors `documents` above. Analysis
    // results get cloned twice on the natural analyze -> export path
    // (once to store here, once when export.rs reads them back out);
    // wrapping in Arc turns both into a cheap refcount bump instead of
    // deep-cloning the batch's facilities/groups/issues each time.
    pub analysis: Option<Arc<AnalysisResults>>,
    pub corrections: HashMap<CorrectionKey, String>,
    pub dimension_exemptions: HashSet<DimensionExemptionKey>,

    /// One resolved vendor-format mapping per file, keyed by file name —
    /// set by `/unit-file/resolve-format` once the user confirms a
    /// detected vendor or manually maps fields. Ephemeral, session-only
    /// (no persistence layer for reusable vendor profiles yet).
    pub format_resolutions: HashMap<String, FieldMapping>,

    /// Explicit user confirmation that the currently selected master
    /// group file (auto-detected or manually uploaded) is the right one
    /// — set only by `/group-file/confirm`. Selecting a *different* file
    /// (`/group-file/upload`) resets this back to `false`; a fresh
    /// selection always needs its own confirmation.
    pub group_file_confirmed: bool,

    /// UnitGroup names to drop entirely from every stage downstream of
    /// `effective_documents` (validation, analysis, export) — set by
    /// `/exclude-group`. Distinct from a correction (which rewrites a
    /// value) or a dimension exemption (which suppresses one check for
    /// one unit): this removes the units as if they were never in the
    /// source file at all.
    pub excluded_groups: HashSet<String>,

    /// (check, group name) pairs a user has accepted "as is" — set by
    /// `/acknowledge-group-warnings`. Unlike `excluded_groups`, this
    /// changes nothing about the data itself; it only suppresses that
    /// one check's flag on that one group going forward. See
    /// `GroupCheckAcknowledgmentKey`.
    pub group_check_acknowledgments: HashSet<GroupCheckAcknowledgmentKey>,

    /// Bumped by every mutation that can change what `effective_documents`
    /// produces (corrections, exemptions, group exclusion/inclusion,
    /// group-check acknowledgment, a re-uploaded document) — see
    /// `Session::touch_data`. `/analyze` and `/export` capture this at
    /// read time and compare it again right before their own delayed
    /// write-back (`complete_analysis`/`complete_export`), so a
    /// correction that lands in the gap between the two can't have its
    /// safety-net stage downgrade silently re-promoted by a slower
    /// analyze/export using data from before the correction.
    pub data_generation: u64,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub metadata: SessionMetadata,
    pub data: SessionData,
    pub workflow: WorkflowStage,
}

impl HasSessionMetadata for Session {
    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut SessionMetadata {
        &mut self.metadata
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StageError {
    pub required: WorkflowStage,
    pub current: WorkflowStage,
}

impl Session {
    pub fn new(id: String, owner_id: Option<Uuid>) -> Self {
        Self {
            metadata: SessionMetadata::new(id, owner_id),
            data: SessionData::default(),
            workflow: WorkflowStage::Uploaded,
        }
    }

    /// The session's parsed documents with any resolved vendor-format
    /// mapping and manual corrections applied, in that order — a file
    /// must be normalized into canonical columns (`Number`/`UnitGroup`/
    /// etc.) before per-cell corrections, which are keyed by the
    /// canonical "number" column, can find anything to match against.
    /// Validation and analysis should read through this instead of
    /// `self.data.documents` directly, so both a format resolution and a
    /// correction made after the initial upload are reflected without
    /// needing to reparse or re-upload anything.
    ///
    /// A file with no *stored* format resolution yet still gets
    /// auto-detected and mapped on the fly rather than passed through
    /// unmapped — this is the single canonical source for "the effective
    /// view of a document," so every caller (including discovery's own
    /// display-only group-name computation) gets the same fallback
    /// instead of each reimplementing it slightly differently.
    pub fn effective_documents(&self) -> Vec<CsvDocument> {
        self.effective_documents_matching(|_| true)
    }

    /// Same as `effective_documents`, but filtered to just the named
    /// documents *before* running the mapping/correction/exclusion
    /// pipeline, instead of transforming every document in the session
    /// (including ones the caller is about to discard, e.g. the master
    /// group file during a unit-file-only operation) and filtering the
    /// result afterward. Every existing caller that filters post hoc
    /// (`analyze.rs`, `validate.rs`, `discover/compute.rs`) can call this
    /// with `discovery.unit_file_names` instead.
    pub fn effective_documents_for(&self, names: &[String]) -> Vec<CsvDocument> {
        self.effective_documents_matching(|document| names.contains(&document.file_name))
    }

    fn effective_documents_matching(
        &self,
        predicate: impl Fn(&CsvDocument) -> bool,
    ) -> Vec<CsvDocument> {
        self.data
            .documents
            .iter()
            .filter(|document| predicate(document))
            .map(
                |document| match self.data.format_resolutions.get(&document.file_name) {
                    Some(mapping) => apply_field_mapping(document, mapping),
                    None => match detect_vendor(document) {
                        Some(vendor) => apply_field_mapping(document, &mapping_from_vendor(vendor)),
                        None => document.clone(),
                    },
                },
            )
            .map(|document| apply_corrections(&document, &self.data.corrections))
            .map(|document| filter_excluded_groups(&document, &self.data.excluded_groups))
            .collect()
    }

    /// Adds a document, replacing any existing one with the same file
    /// name -- used by the manual-file-upload endpoint (see
    /// `api::group_file_upload`), which lets a user designate a specific
    /// file as the group file regardless of how discovery classified
    /// anything already uploaded.
    pub fn upsert_document(&mut self, document: CsvDocument) {
        let documents = Arc::make_mut(&mut self.data.documents);

        match documents
            .iter_mut()
            .find(|d| d.file_name == document.file_name)
        {
            Some(existing) => {
                *existing = document;
            }
            None => {
                documents.push(document);
            }
        }

        self.touch_data();
    }

    pub fn add_correction(&mut self, key: CorrectionKey, value: String) {
        self.data.corrections.insert(key, value);
        self.touch_data();
    }

    pub fn add_dimension_exemption(&mut self, key: DimensionExemptionKey) {
        self.data.dimension_exemptions.insert(key);
        self.touch_data();
    }

    pub fn exclude_group(&mut self, group_name: String) {
        self.data.excluded_groups.insert(group_name);
        self.touch_data();
    }

    pub fn include_group(&mut self, group_name: &str) {
        self.data.excluded_groups.remove(group_name);
        self.touch_data();
    }

    /// Bumps `data_generation` — called by every mutation above that can
    /// change what `effective_documents` produces. See the field's own
    /// doc comment for what this guards against.
    fn touch_data(&mut self) {
        self.data.data_generation = self.data.data_generation.wrapping_add(1);
    }

    pub fn data_generation(&self) -> u64 {
        self.data.data_generation
    }

    /// Counts how many rows in `file_name` currently carry `unit_number`
    /// (after any prior corrections/mapping), trimmed the same way the
    /// unit number is read everywhere else it's used as an identifier.
    /// Used by `/correct` and `/exempt-dimensions` to reject a
    /// `file_name`/`unit_number` pair that doesn't belong to this
    /// session at all (0) as well as one that's ambiguous (2+, since a
    /// duplicate unit number can't be targeted to a single row) --
    /// previously neither handler checked this at all, silently storing
    /// a correction/exemption keyed by a stale, mistyped, or
    /// already-excluded identifier with no error surfaced to the caller.
    pub fn unit_number_occurrences(&self, file_name: &str, unit_number: &str) -> usize {
        let unit_number = unit_number.trim();

        let documents = self.effective_documents();

        let Some(document) = documents.iter().find(|d| d.file_name == file_name) else {
            return 0;
        };

        let Some(number_index) = document.header_index("number") else {
            return 0;
        };

        document
            .rows
            .iter()
            .filter(|row| row.get(number_index).map(|v| v.trim()) == Some(unit_number))
            .count()
    }

    /// Unit numbers exempted from the "Invalid dimensions" check for one
    /// specific file — what `validate_document` should skip that check
    /// for.
    pub fn dimension_exemptions_for(&self, file_name: &str) -> HashSet<String> {
        self.data
            .dimension_exemptions
            .iter()
            .filter(|key| key.file_name == file_name)
            .map(|key| key.unit_number.clone())
            .collect()
    }

    pub fn acknowledge_group_check(&mut self, check: String, group_name: String) {
        self.data
            .group_check_acknowledgments
            .insert(GroupCheckAcknowledgmentKey { check, group_name });
        self.touch_data();
    }

    pub fn unacknowledge_group_check(&mut self, check: &str, group_name: &str) {
        self.data
            .group_check_acknowledgments
            .retain(|key| !(key.check == check && key.group_name == group_name));
        self.touch_data();
    }

    /// Group names accepted "as is" for one specific check (`ODD_UNITGROUP`
    /// or `RARE_GROUP`) — session-wide, not per-file, since a group name is
    /// already a session-wide concept the same way `excluded_groups` is.
    pub fn acknowledged_groups_for(&self, check: &str) -> HashSet<String> {
        self.data
            .group_check_acknowledgments
            .iter()
            .filter(|key| key.check == check)
            .map(|key| key.group_name.clone())
            .collect()
    }

    pub fn require_stage(&self, required: WorkflowStage) -> Result<(), StageError> {
        if self.workflow >= required {
            Ok(())
        } else {
            Err(StageError {
                required,
                current: self.workflow,
            })
        }
    }

    pub fn complete_discovery(&mut self, result: DiscoveryResult) {
        self.data.discovery = Some(result);
        self.workflow = WorkflowStage::Discovered;
    }

    pub fn complete_validation(&mut self, result: ValidationResult) {
        self.data.validation = Some(result);
        self.workflow = WorkflowStage::Validated;
    }

    pub fn complete_analysis(&mut self, result: Arc<AnalysisResults>) {
        self.data.analysis = Some(result);
        self.workflow = WorkflowStage::Analyzed;
    }

    pub fn complete_export(&mut self) {
        self.workflow = WorkflowStage::Exported;
    }
}

#[cfg(test)]
#[path = "unit_group_session_tests.rs"]
mod tests;
