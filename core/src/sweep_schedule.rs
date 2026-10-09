//! When a `DurableSessionStore`'s Postgres expiry sweep next has anything to
//! do.
//!
//! The sweep used to be a fixed 60-second timer per store (five stores), so
//! the server ran a `DELETE` against Postgres about five times a minute,
//! around the clock, whether or not anyone was using it. On a serverless
//! Postgres that scales to zero when idle (Neon suspends compute after a few
//! idle minutes) that timer meant the compute could never suspend. Sweeping
//! only when a row can actually have expired removes the reason to wake the
//! database at all:
//!
//! * a **write** can make a row that expires `timeout` later, so the first
//!   write after a quiet spell schedules one sweep for just after that
//!   expiry;
//! * after each sweep the store asks Postgres for the oldest row still
//!   there and schedules the next sweep for just after *its* expiry;
//! * with no rows left there is nothing scheduled, and nothing runs.
//!
//! This type is only the bookkeeping (what is due, and waking the task that
//! waits on it); it never touches the database, which keeps it testable
//! without one.

use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::time::Instant;

#[derive(Default)]
struct State {
    /// When the next sweep should run; `None` means nothing is scheduled.
    due: Option<Instant>,

    /// A write landed after `due` had already passed but before the sweep
    /// began. That write's row may be inserted after the sweep's own
    /// look at the table, so the sweep must schedule a conservative
    /// follow-up rather than trust what it saw.
    write_in_gap: bool,
}

#[derive(Default)]
pub(crate) struct SweepSchedule {
    state: Mutex<State>,
    wake: Notify,
}

impl SweepSchedule {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// When the next sweep is due, if one is scheduled.
    pub(crate) fn due(&self) -> Option<Instant> {
        self.state.lock().due
    }

    /// Schedules a sweep at `at` unless one is already scheduled sooner, and
    /// wakes the waiting task if that changed anything.
    pub(crate) fn lower_to(&self, at: Instant) {
        let changed = {
            let mut state = self.state.lock();
            match state.due {
                Some(existing) if existing <= at => false,
                _ => {
                    state.due = Some(at);
                    true
                }
            }
        };
        if changed {
            self.wake.notify_one();
        }
    }

    /// A row was just written at `now` and cannot expire before `expires_at`.
    /// Schedules a sweep for then unless one is already scheduled at or
    /// before that (it will cover this row, which expires later), so a sweep
    /// planned for the distant future -- the startup sweep, say -- is pulled
    /// forward by a row that expires sooner.
    pub(crate) fn note_write(&self, now: Instant, expires_at: Instant) {
        let schedule = {
            let mut state = self.state.lock();
            match state.due {
                None => {
                    state.due = Some(expires_at);
                    true
                }
                Some(due) if due <= now => {
                    state.write_in_gap = true;
                    false
                }
                Some(due) if expires_at < due => {
                    state.due = Some(expires_at);
                    true
                }
                Some(_) => false,
            }
        };
        if schedule {
            self.wake.notify_one();
        }
    }

    /// Called as a sweep starts: forgets the schedule (the sweep is about to
    /// act on it) and reports whether a write slipped in after it fell due.
    pub(crate) fn begin_sweep(&self) -> bool {
        let mut state = self.state.lock();
        state.due = None;
        std::mem::take(&mut state.write_in_gap)
    }

    /// Resolves when the schedule changes (or a change happened since the
    /// last call). Callers re-read `due()` afterwards.
    pub(crate) async fn changed(&self) {
        self.wake.notified().await;
    }
}

#[cfg(test)]
#[path = "sweep_schedule_tests.rs"]
mod tests;
