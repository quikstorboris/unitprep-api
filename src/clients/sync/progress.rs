use std::sync::Arc;

use parking_lot::RwLock;

use crate::process_street::ProcessStreetError;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("Process Street request failed: {0}")]
    ProcessStreet(#[from] ProcessStreetError),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncStats {
    pub workflow: &'static str,
    pub runs_seen: usize,
    pub runs_changed: usize,
    pub people_indexed: usize,
    /// Always 0 for every workflow but "intake" -- see `sync_one_run`'s
    /// own doc comment on why company/facility refresh is Intake-only.
    pub companies_refreshed: usize,
    pub facilities_refreshed: usize,
}

/// Shared, pollable progress for whichever sync run is currently in
/// flight (or last finished) -- lets `api::clients_sync`'s "Sync Now"
/// button show a live percentage instead of a bare spinner, and lets the
/// manual trigger and the nightly timer share one "is a sync already
/// running" guard (see `try_claim_running`) so a click during the
/// nightly window doesn't start a second, overlapping pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncState {
    #[default]
    Idle,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Default)]
pub struct SyncProgress {
    pub state: SyncState,
    /// Known only once every workflow's (cheap) run list has come back
    /// -- see `run_all_workflows_with_progress`. Zero while state is
    /// still `Idle`.
    pub total_runs: usize,
    /// Incremented once per run after its delta check resolves, whether
    /// that run was actually refreshed or skipped -- "processed" means
    /// "a decision was made," not "a `form-fields` fetch happened."
    pub processed_runs: usize,
    pub results: Vec<SyncStats>,
    /// Set only when `state == Failed` -- the error that stopped the
    /// run early. A failure on one workflow does not appear here; see
    /// `sync_all_workflows`'s own per-workflow error handling for that
    /// (unrelated) case, still used by the plain, progress-free path.
    pub error: Option<String>,
}

impl SyncProgress {
    pub fn percent(&self) -> u8 {
        if self.total_runs == 0 {
            return 0;
        }
        ((self.processed_runs * 100) / self.total_runs).min(100) as u8
    }
}

pub type SyncProgressHandle = Arc<RwLock<SyncProgress>>;

/// Atomically checks-and-claims the "a sync is running" slot -- `true`
/// means the caller now owns the run and must call
/// `run_all_workflows_with_progress` (which itself sets `Completed`/
/// `Failed` on every exit path); `false` means one was already in
/// progress and the caller should do nothing. The check and the claim
/// happen under the same write lock, so two simultaneous callers (the
/// nightly timer's tick and a manual click landing at the same instant)
/// can't both observe `Idle` and both proceed.
pub fn try_claim_running(progress: &SyncProgressHandle) -> bool {
    let mut guard = progress.write();
    if guard.state == SyncState::Running {
        return false;
    }
    *guard = SyncProgress {
        state: SyncState::Running,
        ..Default::default()
    };
    true
}
