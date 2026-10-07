//! A sliding-window limit on how fast one user's ClickUp calls go out.
//!
//! ClickUp allows about 100 requests a minute per personal token, and
//! every ClickUp Copy call runs on the acting user's own token. A bulk
//! copy to many facilities would blow through that in seconds, and ClickUp
//! answers with 429s that fail rows. So the copy executor takes a slot
//! from this limiter before every ClickUp call: when the window is full it
//! simply waits for the oldest call to age out. The limit sits under
//! ClickUp's own, leaving room for what else the same person is doing
//! (opening the dialog, the duplicate-check panel).
//!
//! One limiter per user, shared by every request and background job that
//! user has running -- two bulk copies at once share one budget.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tokio::time::Instant;
use uuid::Uuid;

/// Calls per user per window, under ClickUp's ~100 a minute.
pub const REQUESTS_PER_WINDOW: usize = 80;

/// The window ClickUp's limit is measured over.
pub const WINDOW: Duration = Duration::from_secs(60);

pub struct RateLimiter {
    limit: usize,
    window: Duration,
    calls: Mutex<VecDeque<Instant>>,
}

impl RateLimiter {
    pub fn new(limit: usize, window: Duration) -> Self {
        Self {
            limit,
            window,
            calls: Mutex::new(VecDeque::new()),
        }
    }

    /// Waits until a call may be made, then counts it. Callers are served
    /// in no particular order, but none waits longer than one window.
    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut calls = self.calls.lock().expect("rate limiter lock");
                let now = Instant::now();
                while calls
                    .front()
                    .is_some_and(|oldest| now.duration_since(*oldest) >= self.window)
                {
                    calls.pop_front();
                }

                if calls.len() < self.limit {
                    calls.push_back(now);
                    return;
                }
                // Full: the oldest call frees a slot when it leaves the window.
                let oldest = *calls.front().expect("a full window has calls");
                (oldest + self.window).saturating_duration_since(now)
            };
            tokio::time::sleep(wait.max(Duration::from_millis(1))).await;
        }
    }

    /// How many calls the window has room for right now.
    #[cfg(test)]
    pub fn available(&self) -> usize {
        let calls = self.calls.lock().expect("rate limiter lock");
        let now = Instant::now();
        let recent = calls
            .iter()
            .filter(|at| now.duration_since(**at) < self.window)
            .count();
        self.limit.saturating_sub(recent)
    }
}

/// The shared limiter for `user_id`'s ClickUp token.
pub fn for_user(user_id: Uuid) -> Arc<RateLimiter> {
    static LIMITERS: OnceLock<Mutex<HashMap<Uuid, Arc<RateLimiter>>>> = OnceLock::new();

    LIMITERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("rate limiter map lock")
        .entry(user_id)
        .or_insert_with(|| Arc::new(RateLimiter::new(REQUESTS_PER_WINDOW, WINDOW)))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn calls_under_the_limit_go_straight_through() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        let started = Instant::now();

        for _ in 0..3 {
            limiter.acquire().await;
        }

        assert_eq!(Instant::now() - started, Duration::ZERO);
        assert_eq!(limiter.available(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_call_over_the_limit_waits_for_the_oldest_to_age_out() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        limiter.acquire().await;
        tokio::time::advance(Duration::from_secs(10)).await;
        limiter.acquire().await;
        let started = Instant::now();

        limiter.acquire().await;

        // The first call (made 10s ago) leaves the window after another 50s.
        assert_eq!(Instant::now() - started, Duration::from_secs(50));
    }

    #[tokio::test(start_paused = true)]
    async fn a_burst_far_over_the_limit_is_spread_across_windows() {
        let limiter = RateLimiter::new(5, Duration::from_secs(60));
        let started = Instant::now();

        for _ in 0..12 {
            limiter.acquire().await;
        }

        // 5 in the first window, 5 in the second, 2 in the third: the last
        // starts a full two windows after the first.
        assert_eq!(Instant::now() - started, Duration::from_secs(120));
    }

    #[tokio::test(start_paused = true)]
    async fn slots_come_back_as_calls_leave_the_window() {
        let limiter = RateLimiter::new(2, Duration::from_secs(60));
        limiter.acquire().await;
        limiter.acquire().await;
        assert_eq!(limiter.available(), 0);

        tokio::time::advance(Duration::from_secs(61)).await;

        assert_eq!(limiter.available(), 2);
    }

    #[test]
    fn each_user_gets_one_shared_limiter() {
        let user = Uuid::new_v4();

        assert!(Arc::ptr_eq(&for_user(user), &for_user(user)));
        assert!(!Arc::ptr_eq(&for_user(user), &for_user(Uuid::new_v4())));
    }
}
