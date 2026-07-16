//! Session orchestration for the duplicate-tenant-check tool. Mirrors
//! `session_service.rs` (UnitGroup's own orchestration) — the binary
//! owns session/HTTP wiring for every tool; `unitprep-dedup` owns only
//! the matching/comparison/note logic, no session state.
//!
//! Deliberately kept out of `domain/` (which today, in practice, means
//! "UnitGroup's own domain logic") rather than adding a second tool's
//! state to a module scoped to the first — see project docs on the
//! still-pending `unit-group` crate extraction.

use std::sync::Arc;

use uuid::Uuid;

use unitprep_core::parsing::parse_document;
use unitprep_core::session::{HasSessionMetadata, SessionMetadata};
use unitprep_core::session_store::SessionStore;
use unitprep_core::uploaded_file::UploadedFile;
use unitprep_dedup::ingest::records_from_csv_document;
use unitprep_dedup::{report, DedupReport, TenantRecord};

/// Only one real stage today: the check runs synchronously on upload,
/// there's no correction loop and no in-app confirm/dismiss step (the
/// MVP scope is "list everything found," corrections happen entirely
/// outside the platform). Kept as an enum, not a bare marker, so a real
/// second stage can be added later without reshaping this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupStage {
    Analyzed,
}

#[derive(Debug, Clone)]
pub struct DedupSession {
    pub metadata: SessionMetadata,
    /// Retained (not discarded after computing `report`) so export can
    /// re-derive tenant details for typo-variant candidates, which only
    /// carry group keys, not the underlying records.
    pub records: Vec<TenantRecord>,
    pub report: DedupReport,

    /// Not read anywhere yet — there's only one stage, and nothing
    /// currently gates on it. Kept (not deleted) as a placeholder for
    /// a real second stage, same rationale as `UploadedFile.relative_path`.
    #[allow(dead_code)]
    pub stage: DedupStage,
}

impl HasSessionMetadata for DedupSession {
    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut SessionMetadata {
        &mut self.metadata
    }
}

impl DedupSession {
    pub fn new(id: String, records: Vec<TenantRecord>, report: DedupReport) -> Self {
        Self {
            metadata: SessionMetadata::new(id),
            records,
            report,
            stage: DedupStage::Analyzed,
        }
    }
}

pub struct DedupSessionService {
    store: Arc<dyn SessionStore<DedupSession>>,
}

impl DedupSessionService {
    pub fn new(store: Arc<dyn SessionStore<DedupSession>>) -> Self {
        Self { store }
    }

    /// Parses, ingests, and analyzes `file` in one step, then stores the
    /// result as a new session. Unlike UnitGroup's multi-file upload
    /// (which tolerates and skips unparseable files), this is a single
    /// QMS export file — a parse/ingest failure here is a real error to
    /// surface to the caller, not something to silently skip.
    pub fn create_session(&self, file: UploadedFile) -> anyhow::Result<String> {
        let document = parse_document(&file)?;
        let records = records_from_csv_document(&document)?;
        let dedup_report = report::run(records.clone());

        let session_id = Uuid::new_v4().to_string();
        let session = DedupSession::new(session_id.clone(), records, dedup_report);

        tracing::info!(
            session_id = %session_id,
            total_rows = session.report.total_rows,
            flagged_groups = session.report.flagged_groups.len(),
            typo_variant_candidates = session.report.typo_variant_candidates.len(),
            "Dedup session created"
        );

        self.store.save(session);

        Ok(session_id)
    }
}
