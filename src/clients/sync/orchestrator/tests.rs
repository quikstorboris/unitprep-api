use chrono::{DateTime, Duration as ChronoDuration, NaiveTime, Utc};
use chrono_tz::Tz;

use super::schedule::*;
use super::*;

#[test]
fn never_synced_before_always_needs_refresh() {
    assert!(needs_refresh(None, Utc::now()));
}

#[test]
fn unchanged_updated_at_does_not_need_refresh() {
    let t = Utc::now();
    assert!(!needs_refresh(Some(t), t));
}

#[test]
fn a_later_updated_at_needs_refresh() {
    let earlier = Utc::now() - ChronoDuration::days(1);
    let later = Utc::now();
    assert!(needs_refresh(Some(earlier), later));
}

#[test]
fn an_updated_at_that_moved_backward_does_not_need_refresh() {
    // Should never happen against the real API, but the comparison
    // itself must not treat "earlier than what's recorded" as a
    // reason to refresh -- only strictly-later does.
    let later = Utc::now();
    let earlier = later - ChronoDuration::days(1);
    assert!(!needs_refresh(Some(later), earlier));
}

#[test]
fn next_daily_occurrence_is_later_today_in_utc_when_the_time_has_not_passed_yet() {
    let now = "2026-08-31T10:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let noon = NaiveTime::from_hms_opt(12, 0, 0).unwrap();

    assert_eq!(
        next_daily_occurrence(now, noon, Tz::UTC),
        "2026-08-31T12:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}

#[test]
fn next_daily_occurrence_rolls_to_tomorrow_when_the_time_has_already_passed_today() {
    let now = "2026-08-31T23:30:00Z".parse::<DateTime<Utc>>().unwrap();
    let ten_pm = NaiveTime::from_hms_opt(22, 0, 0).unwrap();

    assert_eq!(
        next_daily_occurrence(now, ten_pm, Tz::UTC),
        "2026-09-01T22:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}

#[test]
fn next_daily_occurrence_at_the_exact_current_instant_rolls_to_tomorrow_not_zero_sleep() {
    // An exact tie must not be treated as "still ahead" -- sleeping
    // for zero seconds and immediately re-triggering would turn one
    // scheduled sync into a tight loop right at the boundary.
    let now = "2026-08-31T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let midnight = NaiveTime::from_hms_opt(0, 0, 0).unwrap();

    assert_eq!(
        next_daily_occurrence(now, midnight, Tz::UTC),
        "2026-09-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}

#[test]
fn next_daily_occurrence_converts_a_real_timezone_to_the_correct_utc_instant() {
    // 3:00 AM Pacific in late August is PDT (UTC-7) -- 10:00 UTC.
    let now = "2026-08-31T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let three_am = NaiveTime::from_hms_opt(3, 0, 0).unwrap();

    assert_eq!(
        next_daily_occurrence(now, three_am, chrono_tz::America::Los_Angeles),
        "2026-08-31T10:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );
}

#[test]
fn next_daily_occurrence_does_not_panic_across_a_real_spring_forward_gap() {
    // US DST began 2026-03-08 at 02:00 Pacific (clocks jump straight
    // to 03:00) -- 02:30 that day never happened locally. Only
    // asserts this resolves to *something* sane (a real instant,
    // not a panic/unwrap failure) -- the exact chosen instant during
    // a gap is a documented, acceptable imprecision, not a contract.
    let now = "2026-03-08T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
    let two_thirty_am = NaiveTime::from_hms_opt(2, 30, 0).unwrap();

    let next = next_daily_occurrence(now, two_thirty_am, chrono_tz::America::Los_Angeles);
    assert!(next > now);
}
