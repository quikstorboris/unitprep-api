//! Group Prep's (internally still named "UnitGroup" in code) domain
//! logic: discovery-result data, per-facility batch building,
//! exact/fuzzy group-name matching (the fingerprint engine), validation
//! rules, and manual-correction overlays.
//!
//! No session state, HTTP layer, or export format here — those live in
//! the binary (`unitprep`), the same boundary `unitprep-dedup` already
//! established: this crate is pure logic and result data, the binary
//! owns orchestration. `Session`/`WorkflowStage`/`StageError` (the
//! actual stage machine) stay in the binary's `application/` layer,
//! not here — only the pure-data pieces they used to carry
//! (`DiscoveryResult`, `ValidationResult`, `ValidationIssueSummary`)
//! moved over, in `models.rs`.

pub mod analysis;
pub mod corrections;
pub mod models;
pub mod validation;

pub use analysis::{
    analyze_batch,
    build_batch_from_documents,
    load_reference_groups_from_document,
    parse_fingerprint,
    select_group_document,
    Climate,
    GroupFingerprint,
    Location,
};
pub use corrections::{apply_corrections, CorrectionKey, DimensionExemptionKey};
pub use models::{
    AdvisoryIssue,
    AnalysisResults,
    BatchRun,
    DiscoveryResult,
    Facility,
    Issue,
    Severity,
    SimilarityMatch,
    ValidationIssueSummary,
    ValidationResult,
};
pub use validation::{correctable_fields_for, is_dimension_exemptable, validate_document, ValidationIssue};
