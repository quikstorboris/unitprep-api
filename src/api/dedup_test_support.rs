//! Test fixtures for the dedup tool specifically — separate from
//! `test_support.rs` (UnitGroup's own fixtures), matching this
//! project's "new tool gets its own file" pattern rather than growing
//! a shared module to cover two tools' unrelated test data.

use std::sync::Arc;

use unitprep_core::in_memory_session_store::InMemorySessionStore;

use crate::api::test_support::empty_dedup_store;
use crate::api::AppState;
use crate::application::dedup_session_service::DedupSession;
use crate::application::unit_group_session::Session;

/// An `AppState` with one dedup session already populated — what
/// `/dedup/report` and `/dedup/export` need, since `/dedup/check`
/// (the only way to create one for real) takes a multipart body that
/// isn't practical to construct directly in a unit test — same
/// reasoning as why `upload.rs` has no dedicated test file either.
pub fn dedup_state_with_report(
    session_id: &str,
    records: Vec<unitprep_dedup::TenantRecord>,
    report: unitprep_dedup::DedupReport,
) -> AppState {
    let store = empty_dedup_store();
    store.save(DedupSession::new(session_id.to_string(), records, report));

    AppState {
        unit_group_sessions: Arc::new(InMemorySessionStore::<Session>::new()),
        dedup_sessions: store,
    }
}
