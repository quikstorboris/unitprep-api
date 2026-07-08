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
