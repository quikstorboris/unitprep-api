//! A sliding-window limiter on how fast calls to one external service go out.
//!
//! Each service we call has its own rate limit, and going over it gets
//! requests answered with 429s that fail work. A `RateLimiter` is told how
//! many calls fit in a window; every caller takes a slot with `acquire`
//! before making a call, and when the window is full it simply waits for
//! the oldest call to age out. The limit is set under the service's own,
//! leaving room for the other traffic sharing the same budget.
//!
//! Lives at the crate root, not under `clickup`, because more than one
//! integration uses it: ClickUp (one limiter per user token, see
//! `clickup::rate_limit`) and the Process Street sync pacer. Each limiter
//! carries a name, logged as the `limiter` field when a call has to wait,
//! so a throttled call can be told apart by service.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// A named limit of `limit` calls per `window`, shared by every caller
/// holding a reference to it.
pub struct RateLimiter {
    name: &'static str,
    limit: usize,
    window: Duration,
    calls: Mutex<VecDeque<Instant>>,
}

impl RateLimiter {
    pub fn new(name: &'static str, limit: usize, window: Duration) -> Self {
        Self {
            name,
            limit,
            window,
            calls: Mutex::new(VecDeque::new()),
        }
    }

    /// Waits until a call may be made, then counts it. Callers are served
    /// in no particular order, but none waits longer than one window.
    pub async fn acquire(&self) {
        // Logged once per call, not once per wake-up.
        let mut waited = false;
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
                let wait = (oldest + self.window).saturating_duration_since(now);
                if !waited {
                    tracing::warn!(
                        limiter = self.name,
                        wait_ms = wait.as_millis() as u64,
                        limit = self.limit,
                        window_secs = self.window.as_secs(),
                        "Rate limiter full, call is waiting"
                    );
                    waited = true;
                }
                wait
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn calls_under_the_limit_go_straight_through() {
        let limiter = RateLimiter::new("test", 3, Duration::from_secs(60));
        let started = Instant::now();

        for _ in 0..3 {
            limiter.acquire().await;
        }

        assert_eq!(Instant::now() - started, Duration::ZERO);
        assert_eq!(limiter.available(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_call_over_the_limit_waits_for_the_oldest_to_age_out() {
        let limiter = RateLimiter::new("test", 2, Duration::from_secs(60));
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
        let limiter = RateLimiter::new("test", 5, Duration::from_secs(60));
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
        let limiter = RateLimiter::new("test", 2, Duration::from_secs(60));
        limiter.acquire().await;
        limiter.acquire().await;
        assert_eq!(limiter.available(), 0);

        tokio::time::advance(Duration::from_secs(61)).await;

        assert_eq!(limiter.available(), 2);
    }
}
