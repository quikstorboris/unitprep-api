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
    pub analysis: Option<AnalysisResults>,
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
    pub fn new(id: String) -> Self {
        Self {
            metadata: SessionMetadata::new(id),
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
        self.data
            .documents
            .iter()
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
    /// name -- used by the manual-file-upload endpoints (see
    /// `api::unit_file_upload` / `api::group_file_upload`), which let a
    /// user designate a specific file as the unit/group file regardless
    /// of how discovery classified anything already uploaded.
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
    }

    pub fn add_correction(&mut self, key: CorrectionKey, value: String) {
        self.data.corrections.insert(key, value);
    }

    pub fn add_dimension_exemption(&mut self, key: DimensionExemptionKey) {
        self.data.dimension_exemptions.insert(key);
    }

    pub fn exclude_group(&mut self, group_name: String) {
        self.data.excluded_groups.insert(group_name);
    }

    pub fn include_group(&mut self, group_name: &str) {
        self.data.excluded_groups.remove(group_name);
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
    }

    pub fn unacknowledge_group_check(&mut self, check: &str, group_name: &str) {
        self.data
            .group_check_acknowledgments
            .retain(|key| !(key.check == check && key.group_name == group_name));
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

    pub fn complete_analysis(&mut self, result: AnalysisResults) {
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
