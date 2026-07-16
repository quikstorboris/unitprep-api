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
use unitprep_core::session::{
    HasSessionMetadata,
    SessionMetadata,
};
use unitprep_unit_group::{
    apply_corrections,
    AnalysisResults,
    CorrectionKey,
    DimensionExemptionKey,
    DiscoveryResult,
    ValidationResult,
};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
)]
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

    fn metadata_mut(
        &mut self,
    ) -> &mut SessionMetadata {
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

    /// The session's parsed documents with any manual corrections applied.
    /// Validation and analysis should read through this instead of
    /// `self.data.documents` directly, so a correction made after the
    /// initial upload is reflected without needing to reparse or re-upload
    /// anything.
    pub fn effective_documents(
        &self,
    ) -> Vec<CsvDocument> {
        self.data
            .documents
            .iter()
            .map(|document| {
                apply_corrections(
                    document,
                    &self.data.corrections,
                )
            })
            .collect()
    }

    pub fn add_correction(
        &mut self,
        key: CorrectionKey,
        value: String,
    ) {
        self.data
            .corrections
            .insert(key, value);
    }

    pub fn add_dimension_exemption(
        &mut self,
        key: DimensionExemptionKey,
    ) {
        self.data
            .dimension_exemptions
            .insert(key);
    }

    /// Unit numbers exempted from the "Invalid dimensions" check for one
    /// specific file — what `validate_document` should skip that check
    /// for.
    pub fn dimension_exemptions_for(
        &self,
        file_name: &str,
    ) -> HashSet<String> {
        self.data
            .dimension_exemptions
            .iter()
            .filter(|key| {
                key.file_name == file_name
            })
            .map(|key| {
                key.unit_number.clone()
            })
            .collect()
    }

    pub fn require_stage(
        &self,
        required: WorkflowStage,
    ) -> Result<(), StageError> {
        if self.workflow >= required {
            Ok(())
        } else {
            Err(StageError {
                required,
                current: self.workflow,
            })
        }
    }

    pub fn complete_discovery(
        &mut self,
        result: DiscoveryResult,
    ) {
        self.data.discovery = Some(result);
        self.workflow =
            WorkflowStage::Discovered;
    }

    pub fn complete_validation(
        &mut self,
        result: ValidationResult,
    ) {
        self.data.validation = Some(result);
        self.workflow =
            WorkflowStage::Validated;
    }

    pub fn complete_analysis(
        &mut self,
        result: AnalysisResults,
    ) {
        self.data.analysis = Some(result);
        self.workflow =
            WorkflowStage::Analyzed;
    }

    pub fn complete_export(
        &mut self,
    ) {
        self.workflow =
            WorkflowStage::Exported;
    }
}

#[cfg(test)]
#[path = "unit_group_session_tests.rs"]
mod tests;
