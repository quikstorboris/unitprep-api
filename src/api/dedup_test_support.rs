//! Test fixtures for the dedup tool specifically — separate from
//! `test_support.rs` (UnitGroup's own fixtures), matching this
//! project's "new tool gets its own file" pattern rather than growing
//! a shared module to cover two tools' unrelated test data.

use std::sync::Arc;

use unitprep_core::in_memory_session_store::InMemorySessionStore;

use crate::api::test_support::empty_auth_ceremony_store;
use crate::api::test_support::empty_ceremony_store;
use crate::api::test_support::empty_dedup_store;
use crate::api::test_support::empty_tagger_store;
use crate::api::test_support::empty_vendor_cache;
use crate::api::test_support::test_auth_backend;
use crate::api::test_support::test_db_pool;
use crate::api::test_support::test_user_id;
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
    dedup_state_with_source_folder(session_id, records, report, None)
}

/// Same as `dedup_state_with_report`, but lets a test control
/// `source_dropbox_folder_path` directly -- what `api::dedup::
/// save_location`'s own tests need to prove the Dropbox-imported vs.
/// locally-uploaded cases separately.
pub fn dedup_state_with_source_folder(
    session_id: &str,
    records: Vec<unitprep_dedup::TenantRecord>,
    report: unitprep_dedup::DedupReport,
    source_dropbox_folder_path: Option<String>,
) -> AppState {
    let store = empty_dedup_store();
    store.save(DedupSession::new(
        session_id.to_string(),
        Some(test_user_id()),
        records,
        report,
        source_dropbox_folder_path,
    ));

    AppState {
        unit_group_sessions: Arc::new(InMemorySessionStore::<Session>::new()),
        dedup_sessions: store,
        tagger_sessions: empty_tagger_store(),
        db: test_db_pool(),
        auth_backend: test_auth_backend(),
        registration_ceremonies: empty_ceremony_store(),
        authentication_ceremonies: empty_auth_ceremony_store(),
        unit_vendors: empty_vendor_cache(),
        tenant_vendors: empty_vendor_cache(),
        dropbox: crate::api::test_support::test_dropbox_client(),
        process_street: None,
        sync_progress: crate::api::test_support::test_sync_progress(),
    }
}
