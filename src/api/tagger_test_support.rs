//! Test fixtures for the QMS Template Tagging Assistant specifically --
//! separate from `test_support.rs`, matching this project's "new tool
//! gets its own file" pattern already used by `dedup_test_support.rs`.

use std::sync::Arc;

use unitprep_core::in_memory_session_store::InMemorySessionStore;
use unitprep_tagger_pipeline::RegionCandidate;

use crate::api::test_support::empty_auth_ceremony_store;
use crate::api::test_support::empty_ceremony_store;
use crate::api::test_support::empty_dedup_store;
use crate::api::test_support::empty_tagger_store;
use crate::api::test_support::test_auth_backend;
use crate::api::test_support::test_db_pool;
use crate::api::AppState;
use crate::application::tagger_session_service::TaggerSession;
use crate::application::unit_group_session::Session;

/// An `AppState` with one tagger session already populated -- what
/// `/tagger/report` and `/tagger/apply` need, since `/tagger/check`
/// (the only way to create one for real) takes a multipart body that
/// isn't practical to construct directly in a unit test -- same
/// reasoning `dedup_test_support.rs` already documents for dedup.
pub fn tagger_state_with_session(
    session_id: &str,
    original_bytes: Vec<u8>,
    original_file_name: &str,
    candidates: Vec<RegionCandidate>,
) -> AppState {
    let store = empty_tagger_store();
    store.save(TaggerSession::new(
        session_id.to_string(),
        None,
        original_bytes,
        original_file_name.to_string(),
        candidates,
    ));

    AppState {
        unit_group_sessions: Arc::new(InMemorySessionStore::<Session>::new()),
        dedup_sessions: empty_dedup_store(),
        tagger_sessions: store,
        db: test_db_pool(),
        auth_backend: test_auth_backend(),
        registration_ceremonies: empty_ceremony_store(),
        authentication_ceremonies: empty_auth_ceremony_store(),
    }
}
