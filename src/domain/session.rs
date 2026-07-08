use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::SystemTime;

use crate::domain::corrections::{
    apply_corrections,
    CorrectionKey,
    DimensionExemptionKey,
};
use crate::domain::csv_document::CsvDocument;
use crate::domain::models::{
    AnalysisResults,
    Severity,
};
use serde::Serialize;

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

#[derive(Debug, Clone)]
pub struct SessionMetadata {
    pub id: String,
    pub created_at: SystemTime,
    pub last_accessed: SystemTime,
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

#[derive(Debug, Clone, Copy)]
pub struct StageError {
    pub required: WorkflowStage,
    pub current: WorkflowStage,
}

impl Session {
    pub fn new(id: String) -> Self {
        let now = SystemTime::now();

        Self {
            metadata: SessionMetadata {
                id,
                created_at: now,
                last_accessed: now,
            },
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

#[derive(Debug, Clone)]
pub struct DiscoveryResult {
    pub unit_file_names: Vec<String>,
    pub group_file_names: Vec<String>,
    pub selected_group_file_name:
        Option<String>,
    pub ready: bool,
}

#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub files_checked: usize,

    pub issue_count: usize,

    pub error_count: usize,

    pub warning_count: usize,

    pub issues:
        Vec<ValidationIssueSummary>,

    pub ready: bool,
}

#[derive(
    Debug,
    Clone,
    Serialize,
)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::models::{
        AnalysisResults,
        BatchRun,
    };

    fn discovery_result(
    ) -> DiscoveryResult {
        DiscoveryResult {
            unit_file_names: vec![
                "units.csv"
                    .to_string(),
            ],
            group_file_names: vec![
                "groups.csv"
                    .to_string(),
            ],
            selected_group_file_name:
                Some(
                    "groups.csv"
                        .to_string(),
                ),
            ready: true,
        }
    }

    fn validation_result(
    ) -> ValidationResult {
        ValidationResult {
            files_checked: 1,
            issue_count: 0,
            error_count: 0,
            warning_count: 0,
            issues: Vec::new(),
            ready: true,
        }
    }

    fn analysis_results(
    ) -> AnalysisResults {
        AnalysisResults {
            batch_run: BatchRun {
                facilities:
                    Vec::new(),
                global_groups:
                    Default::default(),
                advisory_issues:
                    Vec::new(),
            },
            reference_groups:
                None,
            net_new_groups:
                Vec::new(),
            similar_groups:
                Vec::new(),
        }
    }

    #[test]
    fn new_session_starts_uploaded() {
        let session =
            Session::new(
                "s1".to_string(),
            );

        assert_eq!(
            session.workflow,
            WorkflowStage::Uploaded
        );

        assert!(
            session
                .require_stage(
                    WorkflowStage::Uploaded
                )
                .is_ok()
        );

        assert!(
            session
                .require_stage(
                    WorkflowStage::Discovered
                )
                .is_err()
        );
    }

    #[test]
    fn stage_ordering_is_pipeline_order(
    ) {
        assert!(
            WorkflowStage::Uploaded
                < WorkflowStage::Discovered
        );

        assert!(
            WorkflowStage::Discovered
                < WorkflowStage::Validated
        );

        assert!(
            WorkflowStage::Validated
                < WorkflowStage::Analyzed
        );

        assert!(
            WorkflowStage::Analyzed
                < WorkflowStage::Exported
        );
    }

    #[test]
    fn complete_discovery_advances_stage_and_stores_data(
    ) {
        let mut session =
            Session::new(
                "s1".to_string(),
            );

        session.complete_discovery(
            discovery_result(),
        );

        assert_eq!(
            session.workflow,
            WorkflowStage::Discovered
        );

        assert!(
            session
                .data
                .discovery
                .is_some()
        );
    }

    #[test]
    fn require_stage_reports_current_stage_on_failure(
    ) {
        let mut session =
            Session::new(
                "s1".to_string(),
            );

        session.complete_discovery(
            discovery_result(),
        );

        let err = session
            .require_stage(
                WorkflowStage::Analyzed,
            )
            .unwrap_err();

        assert_eq!(
            err.required,
            WorkflowStage::Analyzed
        );

        assert_eq!(
            err.current,
            WorkflowStage::Discovered
        );
    }

    #[test]
    fn full_pipeline_progression_reaches_exported(
    ) {
        let mut session =
            Session::new(
                "s1".to_string(),
            );

        session.complete_discovery(
            discovery_result(),
        );

        session.complete_validation(
            validation_result(),
        );

        session.complete_analysis(
            analysis_results(),
        );

        session.complete_export();

        assert_eq!(
            session.workflow,
            WorkflowStage::Exported
        );

        assert!(
            session
                .data
                .discovery
                .is_some()
        );

        assert!(
            session
                .data
                .validation
                .is_some()
        );

        assert!(
            session
                .data
                .analysis
                .is_some()
        );
    }
}