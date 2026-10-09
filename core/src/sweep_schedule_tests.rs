use std::time::Duration;

use tokio::time::Instant;

use super::*;

fn after(base: Instant, secs: u64) -> Instant {
    base + Duration::from_secs(secs)
}

#[test]
fn nothing_is_scheduled_until_something_is_written() {
    assert!(SweepSchedule::new().due().is_none());
}

#[test]
fn the_first_write_schedules_a_sweep_at_that_rows_expiry() {
    let schedule = SweepSchedule::new();
    let now = Instant::now();

    schedule.note_write(now, after(now, 100));

    assert_eq!(schedule.due(), Some(after(now, 100)));
}

#[test]
fn a_later_write_never_pushes_an_earlier_sweep_back() {
    let schedule = SweepSchedule::new();
    let now = Instant::now();

    schedule.note_write(now, after(now, 100));
    schedule.note_write(after(now, 40), after(now, 140));

    assert_eq!(
        schedule.due(),
        Some(after(now, 100)),
        "rows written later expire later, so the earlier sweep still covers the oldest one"
    );
}

#[test]
fn a_row_that_expires_before_the_scheduled_sweep_pulls_it_forward() {
    let schedule = SweepSchedule::new();
    let now = Instant::now();
    schedule.lower_to(after(now, 600)); // e.g. the startup sweep

    schedule.note_write(now, after(now, 100));

    assert_eq!(
        schedule.due(),
        Some(after(now, 100)),
        "the distant sweep must not leave this row unswept past its expiry"
    );
}

#[test]
fn lower_to_only_ever_moves_the_schedule_earlier() {
    let schedule = SweepSchedule::new();
    let now = Instant::now();

    schedule.lower_to(after(now, 50));
    schedule.lower_to(after(now, 90));
    assert_eq!(schedule.due(), Some(after(now, 50)));

    schedule.lower_to(after(now, 20));
    assert_eq!(schedule.due(), Some(after(now, 20)));
}

#[test]
fn beginning_a_sweep_clears_the_schedule() {
    let schedule = SweepSchedule::new();
    let now = Instant::now();
    schedule.lower_to(after(now, 10));

    assert!(!schedule.begin_sweep());

    assert!(schedule.due().is_none());
}

#[test]
fn a_write_after_the_sweep_fell_due_is_reported_by_the_sweep_that_follows() {
    let schedule = SweepSchedule::new();
    let now = Instant::now();
    schedule.lower_to(now);

    // The sweep is due (at <= now) but has not begun: this write's row may
    // land after the sweep's own look at the table.
    schedule.note_write(after(now, 1), after(now, 101));

    assert_eq!(
        schedule.due(),
        Some(now),
        "the overdue sweep is not pushed later"
    );
    assert!(
        schedule.begin_sweep(),
        "the sweep must be told a write slipped in, so it schedules a follow-up"
    );
    assert!(!schedule.begin_sweep(), "reported once, then forgotten");
}

#[test]
fn a_write_during_a_sweep_schedules_the_next_one() {
    let schedule = SweepSchedule::new();
    let now = Instant::now();
    schedule.lower_to(now);
    schedule.begin_sweep();

    schedule.note_write(after(now, 2), after(now, 102));

    assert_eq!(schedule.due(), Some(after(now, 102)));
}

#[tokio::test]
async fn a_scheduled_sweep_wakes_a_task_that_is_waiting_for_one() {
    let schedule = std::sync::Arc::new(SweepSchedule::new());
    let waiter = {
        let schedule = schedule.clone();
        tokio::spawn(async move {
            schedule.changed().await;
            schedule.due()
        })
    };
    tokio::task::yield_now().await;

    let now = Instant::now();
    schedule.note_write(now, after(now, 60));

    let seen = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("the waiter must be woken")
        .unwrap();
    assert_eq!(seen, Some(after(now, 60)));
}
