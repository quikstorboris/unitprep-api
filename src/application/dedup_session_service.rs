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
use unitprep_core::vendor_format::VendorFormat;
use unitprep_dedup::ingest::records_from_csv_document;
use unitprep_dedup::{report, DedupReport, TenantRecord};

/// Only one real stage today: the check runs synchronously on upload,
/// there's no correction loop and no in-app confirm/dismiss step (the
/// MVP scope is "list everything found," corrections happen entirely
/// outside the platform). Kept as an enum, not a bare marker, so a real
/// second stage can be added later without reshaping this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DedupStage {
    Analyzed,
}

/// `Serialize`/`Deserialize` are derived so this type can round-trip
/// through `unitprep_core::durable_session_store::DurableSessionStore` --
/// same rationale as `RegistrationCeremony` (see that type's own doc
/// comment). Every field here is already a plain data/domain type with no
/// special (de)serialization needs: `TenantRecord`/`DedupReport` (and
/// everything they're built from, in the `unitprep_dedup` crate) are pure
/// data with no `Arc`/raw-handle fields, so their own derives needed no
/// extra serde feature flags.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DedupSession {
    pub metadata: SessionMetadata,
    /// Retained (not discarded after computing `report`) so export can
    /// re-derive tenant details for typo-variant candidates, which only
    /// carry group keys, not the underlying records.
    pub records: Vec<TenantRecord>,
    pub report: DedupReport,

    /// The Dropbox folder the source file was imported from (its parent
    /// directory) -- `None` for a locally-uploaded file, which has no
    /// Dropbox origin to anchor a save-location default to. Lets
    /// `api::dedup`'s save-to-Dropbox flow default to a `Duplicate Check`
    /// subfolder next to wherever the analyzed file actually came from,
    /// instead of always asking the user to browse from scratch (Boris,
    /// 2026-09-04: two possible source folders in practice -- "Prelim
    /// Check"/"Final Check" or whatever a given facility happens to call
    /// them -- so this remembers the real folder rather than trying to
    /// pattern-match a name).
    pub source_dropbox_folder_path: Option<String>,

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
    pub fn new(
        id: String,
        owner_id: Option<Uuid>,
        records: Vec<TenantRecord>,
        report: DedupReport,
        source_dropbox_folder_path: Option<String>,
    ) -> Self {
        Self {
            metadata: SessionMetadata::new(id, owner_id),
            records,
            report,
            source_dropbox_folder_path,
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
    /// Returns the freshly built report and records alongside the new
    /// session id, so the caller can use them directly rather than
    /// immediately re-fetching (and re-cloning) the very session just
    /// saved below.
    pub fn create_session(
        &self,
        file: UploadedFile,
        owner_id: Option<Uuid>,
        tenant_vendors: &[VendorFormat],
        source_dropbox_folder_path: Option<String>,
    ) -> anyhow::Result<(String, DedupReport, Vec<TenantRecord>)> {
        let document = parse_document(&file)?;
        let records = records_from_csv_document(&document, tenant_vendors)?;
        let dedup_report = report::run(records.clone());

        let session_id = Uuid::new_v4().to_string();
        let session = DedupSession::new(
            session_id.clone(),
            owner_id,
            records.clone(),
            dedup_report.clone(),
            source_dropbox_folder_path,
        );

        tracing::info!(
            session_id = %session_id,
            total_rows = session.report.total_rows,
            flagged_groups = session.report.flagged_groups.len(),
            typo_variant_candidates = session.report.typo_variant_candidates.len(),
            "Dedup session created"
        );

        self.store.save(session);

        Ok((session_id, dedup_report, records))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::Duration;

    use unitprep_core::durable_session_store::DurableSessionStore;
    use unitprep_core::session_store::SessionStore;

    fn record(
        first_last: &str,
        first_name: &str,
        last_name: &str,
        unit: &str,
        email: &str,
        phone: &str,
    ) -> TenantRecord {
        TenantRecord {
            first_last: first_last.to_string(),
            first_name: first_name.to_string(),
            last_name: last_name.to_string(),
            unit_number: unit.to_string(),
            email: email.to_string(),
            phone_number: phone.to_string(),
            ..Default::default()
        }
    }

    /// Integration test for `DedupSession` surviving a process restart --
    /// the `unitprep_dedup`-flavored sibling of `registration_ceremony.rs`'s
    /// own `a_registration_ceremony_survives_a_simulated_process_restart_
    /// durability` test. See that test's doc comment for the full
    /// rationale (why a real DB, why this project verifies empirically
    /// rather than by reading code alone) -- it applies identically here,
    /// just against a session type carrying a whole tool's analysis
    /// output (`records`/`report`) instead of an opaque webauthn-rs
    /// blob.
    ///
    /// The fixture data actually runs through `unitprep_dedup::report::run`
    /// rather than being hand-built, so `dedup_report` carries a real
    /// `FlaggedGroup` (with real mismatches/note text) -- confirming the
    /// full nested `TenantRecord`/`FlaggedGroup`/`FieldMismatch`/
    /// `FieldValueMismatch` chain round-trips through bincode, not just
    /// `DedupReport`'s own top-level counters.
    ///
    /// `metadata.owner_id` is `None` for the same reason
    /// `registration_ceremony.rs`'s test leaves it `None` -- see that
    /// test's own doc comment.
    ///
    /// Run explicitly with `cargo test -- --ignored durability`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a real, reachable Postgres with migrations applied -- see registration_ceremony.rs's equivalent test for the full rationale"]
    async fn a_dedup_session_survives_a_simulated_process_restart_durability() {
        let _ = dotenvy::from_filename(".env.local");

        let db =
            crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");

        // A unique kind per test run -- keeps this test's row fully
        // isolated from a concurrently-running real server instance
        // persisting under the real "dedup_session" kind, and makes this
        // test safely repeatable/parallelizable with itself. Same
        // rationale as registration_ceremony.rs's own test.
        let kind = format!("test_dedup_session_{}", Uuid::new_v4());
        let session_id = Uuid::new_v4().to_string();

        // "Smith, John" -- two units, same tenant, one has an email on
        // file and the other doesn't: a real contact-info mismatch that
        // `report::run` will surface in `flagged_groups`. "Maria Garcia"
        // is just a second, unrelated single-unit tenant so `records`
        // carries more than one group.
        let records = vec![
            record(
                "Smith, John",
                "John",
                "Smith",
                "A1",
                "john@example.com",
                "5551110001",
            ),
            record("Smith, John", "John", "Smith", "A2", "", "5551110001"),
            record(
                "Maria Garcia",
                "Maria",
                "Garcia",
                "C1",
                "maria@example.com",
                "5559876543",
            ),
        ];

        let dedup_report = report::run(records.clone());
        assert!(
            !dedup_report.flagged_groups.is_empty(),
            "fixture must produce at least one flagged group for this test to be meaningful"
        );

        let session = DedupSession::new(
            session_id.clone(),
            None,
            records.clone(),
            dedup_report.clone(),
            Some("/Acme Storage/Prelim Check".to_string()),
        );

        let original_records = session.records.clone();
        let original_report = session.report.clone();
        let original_source_folder = session.source_dropbox_folder_path.clone();
        let original_stage = session.stage;

        let store_before_restart = DurableSessionStore::<DedupSession>::with_timeout(
            db.clone(),
            kind.clone(),
            Duration::from_secs(5 * 60),
        );

        store_before_restart.save(session);

        // save()'s Postgres write is fire-and-forget -- poll briefly for
        // the row to actually land before simulating a restart, same as
        // registration_ceremony.rs's own test.
        let mut persisted = false;

        for _ in 0..20 {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM auth.durable_sessions WHERE kind = $1 AND id = $2",
            )
            .bind(&kind)
            .bind(&session_id)
            .fetch_one(&db)
            .await
            .expect("querying auth.durable_sessions must not fail");

            if count == 1 {
                persisted = true;
                break;
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        assert!(
            persisted,
            "save() must write a row to auth.durable_sessions within ~2 seconds"
        );

        // Simulate a process restart: drop the store entirely and build a
        // brand new one against the SAME underlying Postgres connection
        // pool.
        drop(store_before_restart);

        let store_after_restart = DurableSessionStore::<DedupSession>::with_timeout(
            db.clone(),
            kind.clone(),
            Duration::from_secs(5 * 60),
        );

        let handle = store_after_restart.get_handle(&session_id).expect(
            "get_handle must rehydrate the dedup session from Postgres after a simulated restart",
        );

        {
            let rehydrated = handle.read();

            assert_eq!(rehydrated.metadata.id, session_id);
            assert_eq!(rehydrated.metadata.owner_id, None);
            assert!(!rehydrated.metadata.cancelled);

            assert_eq!(rehydrated.records.len(), original_records.len());
            for (actual, expected) in rehydrated.records.iter().zip(original_records.iter()) {
                assert_eq!(actual.first_last, expected.first_last);
                assert_eq!(actual.unit_number, expected.unit_number);
                assert_eq!(actual.email, expected.email);
                assert_eq!(actual.phone_number, expected.phone_number);
            }

            assert_eq!(rehydrated.report.total_rows, original_report.total_rows);
            assert_eq!(
                rehydrated.report.unique_tenants,
                original_report.unique_tenants
            );
            assert_eq!(
                rehydrated.report.flagged_groups.len(),
                original_report.flagged_groups.len()
            );
            assert_eq!(
                rehydrated.report.flagged_groups[0].note,
                original_report.flagged_groups[0].note
            );
            assert_eq!(
                rehydrated.report.flagged_groups[0].mismatches.len(),
                original_report.flagged_groups[0].mismatches.len()
            );
            assert_eq!(
                rehydrated.report.flagged_groups[0].group.records.len(),
                original_report.flagged_groups[0].group.records.len()
            );

            assert_eq!(
                rehydrated.source_dropbox_folder_path,
                original_source_folder
            );
            assert_eq!(rehydrated.stage, original_stage);
        }

        // Clean up -- leave no row behind for the next run.
        store_after_restart.delete(&session_id);

        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}
