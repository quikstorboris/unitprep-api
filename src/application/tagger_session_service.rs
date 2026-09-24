//! Session orchestration for the QMS Template Tagging Assistant. Mirrors
//! `dedup_session_service.rs` -- the binary owns session/HTTP wiring for
//! every tool; `unitprep-tagger-pipeline` (and the two crates it wires
//! together) own only the matching logic, no session state.
//!
//! Only one real stage today, same as dedup: candidates are found on
//! upload, there's no separate "analyze" step to wait for.

use std::sync::Arc;

use uuid::Uuid;

use unitprep_core::session::{HasSessionMetadata, SessionMetadata};
use unitprep_core::session_store::SessionStore;
use unitprep_tagger_pipeline::RegionCandidate;

/// `Serialize`/`Deserialize` are derived so this type can round-trip
/// through `unitprep_core::durable_session_store::DurableSessionStore`
/// (see `main.rs` for where the store itself is wired up) -- every field
/// here is already a plain, serde-friendly shape (`Vec<u8>`, `String`,
/// `Option<String>`) except `candidates`, whose `RegionCandidate` (and
/// everything it's built from, across `unitprep_tagger_pipeline`,
/// `unitprep_template_tagger`, and `docx_surgeon`) now derives
/// `Serialize`/`Deserialize` too for the same reason.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaggerSession {
    pub metadata: SessionMetadata,
    /// Retained (not discarded after finding candidates) so `/apply` can
    /// re-derive the document's regions and splice confirmed edits into
    /// the ORIGINAL bytes -- re-parsing is cheap and keeps this session
    /// from needing to store a second, possibly-drifted copy of the
    /// flattened document alongside it.
    pub original_bytes: Vec<u8>,
    pub original_file_name: String,
    pub candidates: Vec<RegionCandidate>,
    /// The Dropbox folder the original file was imported from (its
    /// parent directory) -- `None` for a locally-uploaded file. Mirrors
    /// `DedupSession::source_dropbox_folder_path`; see that field's own
    /// doc comment for why this exists (a "Duplicate Check"-equivalent
    /// default save location next to wherever the source came from).
    pub source_dropbox_folder_path: Option<String>,
}

impl HasSessionMetadata for TaggerSession {
    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut SessionMetadata {
        &mut self.metadata
    }
}

impl TaggerSession {
    pub fn new(
        id: String,
        owner_id: Option<Uuid>,
        original_bytes: Vec<u8>,
        original_file_name: String,
        candidates: Vec<RegionCandidate>,
        source_dropbox_folder_path: Option<String>,
    ) -> Self {
        Self {
            metadata: SessionMetadata::new(id, owner_id),
            original_bytes,
            original_file_name,
            candidates,
            source_dropbox_folder_path,
        }
    }
}

pub struct TaggerSessionService {
    store: Arc<dyn SessionStore<TaggerSession>>,
}

impl TaggerSessionService {
    pub fn new(store: Arc<dyn SessionStore<TaggerSession>>) -> Self {
        Self { store }
    }

    /// Stores an already-recognized document as a new session. Finding
    /// the candidates themselves happens in the HTTP handler, not here --
    /// unlike dedup's ingest step, it needs a DB round trip (the active
    /// pattern library) that this service has no business owning.
    pub fn create_session(
        &self,
        original_bytes: Vec<u8>,
        original_file_name: String,
        candidates: Vec<RegionCandidate>,
        owner_id: Option<Uuid>,
        source_dropbox_folder_path: Option<String>,
    ) -> String {
        let session_id = Uuid::new_v4().to_string();
        let session = TaggerSession::new(
            session_id.clone(),
            owner_id,
            original_bytes,
            original_file_name,
            candidates,
            source_dropbox_folder_path,
        );

        tracing::info!(
            session_id = %session_id,
            candidate_count = session.candidates.len(),
            "Tagger session created"
        );

        self.store.save(session);

        session_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Integration test for `TaggerSession` surviving a process restart via
    /// `unitprep_core::durable_session_store::DurableSessionStore` -- same
    /// pattern as `auth::registration_ceremony`'s own
    /// `a_registration_ceremony_survives_a_simulated_process_restart_durability`
    /// test (see that test's doc comment for the full rationale: this
    /// project verifies durability empirically against a real database,
    /// not just by reading the wrapper's code). `main.rs` still wires
    /// `TaggerSession` to a plain `InMemorySessionStore` today -- that
    /// wiring change is a separate, later pass (see this crate's own
    /// module doc comment) -- so this test builds its own
    /// `DurableSessionStore<TaggerSession>` directly rather than going
    /// through `TaggerSessionService`.
    ///
    /// Uses realistic, non-empty values for every field, including a
    /// `RegionCandidate` built from real `docx-surgeon`/
    /// `unitprep-template-tagger` types, and asserts a byte-for-byte match
    /// on `original_bytes` specifically -- bincode's whole reason for
    /// existing here (see `DurableSessionStore`'s own doc comment) is
    /// exact byte round-tripping of exactly this kind of field.
    ///
    /// Run explicitly with `cargo test -- --ignored tagger_session_survives`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
    async fn a_tagger_session_survives_a_simulated_process_restart_durability() {
        use docx_surgeon::RegionRef;
        use unitprep_core::durable_session_store::DurableSessionStore;
        use unitprep_tagger_pipeline::ConfidenceTier;
        use unitprep_template_tagger::Candidate;

        let _ = dotenvy::from_filename(".env.local");

        let db =
            crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");

        // A unique kind per test run -- keeps this test's row fully
        // isolated from anything a concurrently-running real server
        // instance might be persisting under the real
        // "tagger_session" kind, and makes this test safely repeatable/
        // parallelizable with itself. Same rationale as
        // registration_ceremony's own durability test.
        let kind = format!("test_tagger_session_{}", Uuid::new_v4());
        let session_id = format!("test-tagger-session-{}", Uuid::new_v4());

        let original_bytes: Vec<u8> = (0u16..2000).map(|n| (n % 256) as u8).collect();
        let original_file_name = "Move-In Package.docx".to_string();
        let candidates = vec![
            RegionCandidate {
                region: RegionRef::Body,
                candidate: Candidate {
                    tag_key: "e.name".to_string(),
                    matched_text: "John Smith".to_string(),
                    start: 8,
                    end: 18,
                },
                tier: ConfidenceTier::Auto,
            },
            RegionCandidate {
                region: RegionRef::TableCell(2),
                candidate: Candidate {
                    tag_key: "u.num".to_string(),
                    matched_text: "204".to_string(),
                    start: 0,
                    end: 3,
                },
                tier: ConfidenceTier::NeedsReview,
            },
        ];
        let source_dropbox_folder_path = Some("/Facility Docs/Templates".to_string());

        let session = TaggerSession::new(
            session_id.clone(),
            None,
            original_bytes.clone(),
            original_file_name.clone(),
            candidates.clone(),
            source_dropbox_folder_path.clone(),
        );

        let store_before_restart = DurableSessionStore::<TaggerSession>::with_timeout(
            db.clone(),
            kind.clone(),
            std::time::Duration::from_secs(5 * 60),
        );

        store_before_restart.save(session);

        // save()'s Postgres write is fire-and-forget (see
        // DurableSessionStore::persist's own doc comment) -- poll briefly
        // for the row to actually land before simulating a restart.
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

            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        assert!(
            persisted,
            "save() must write a row to auth.durable_sessions within ~2 seconds"
        );

        // Simulate a process restart: drop the store entirely (its
        // in-memory layer, and every handle into it, goes with it) and
        // build a brand new one against the SAME underlying Postgres
        // connection pool.
        drop(store_before_restart);

        let store_after_restart = DurableSessionStore::<TaggerSession>::with_timeout(
            db.clone(),
            kind.clone(),
            std::time::Duration::from_secs(5 * 60),
        );

        let handle = store_after_restart.get_handle(&session_id).expect(
            "get_handle must rehydrate the session from Postgres after a simulated restart",
        );

        {
            let rehydrated = handle.read();

            assert_eq!(rehydrated.metadata.id, session_id);
            assert_eq!(rehydrated.metadata.owner_id, None);
            assert!(!rehydrated.metadata.cancelled);
            assert_eq!(rehydrated.original_bytes, original_bytes);
            assert_eq!(rehydrated.original_file_name, original_file_name);
            assert_eq!(rehydrated.candidates, candidates);
            assert_eq!(
                rehydrated.source_dropbox_folder_path,
                source_dropbox_folder_path
            );
        }

        // Clean up -- leave no row behind for the next run.
        store_after_restart.delete(&session_id);

        // delete()'s Postgres write is ALSO fire-and-forget -- give it a
        // moment before the test process exits so the row is actually
        // gone rather than merely scheduled to be.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
}
