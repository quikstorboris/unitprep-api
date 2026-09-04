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

#[derive(Debug, Clone)]
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
