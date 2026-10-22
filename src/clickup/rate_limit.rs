//! The ClickUp side of rate limiting: one shared limiter per user.
//!
//! ClickUp allows about 100 requests a minute per personal token, and
//! every ClickUp Copy call runs on the acting user's own token. A bulk
//! copy to many facilities would blow through that in seconds, and ClickUp
//! answers with 429s that fail rows. So the copy executor takes a slot
//! from the user's limiter before every ClickUp call. The limit sits under
//! ClickUp's own, leaving room for what else the same person is doing
//! (opening the dialog, the duplicate-check panel).
//!
//! One limiter per user, shared by every request and background job that
//! user has running -- two bulk copies at once share one budget. The
//! limiter itself is the generic [`crate::rate_limit::RateLimiter`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use uuid::Uuid;

use crate::rate_limit::RateLimiter;

/// Calls per user per window, under ClickUp's ~100 a minute.
pub const REQUESTS_PER_WINDOW: usize = 80;

/// The window ClickUp's limit is measured over.
pub const WINDOW: Duration = Duration::from_secs(60);

/// The shared limiter for `user_id`'s ClickUp token.
pub fn for_user(user_id: Uuid) -> Arc<RateLimiter> {
    static LIMITERS: OnceLock<Mutex<HashMap<Uuid, Arc<RateLimiter>>>> = OnceLock::new();

    LIMITERS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("rate limiter map lock")
        .entry(user_id)
        .or_insert_with(|| Arc::new(RateLimiter::new("clickup", REQUESTS_PER_WINDOW, WINDOW)))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_user_gets_one_shared_limiter() {
        let user = Uuid::new_v4();

        assert!(Arc::ptr_eq(&for_user(user), &for_user(user)));
        assert!(!Arc::ptr_eq(&for_user(user), &for_user(Uuid::new_v4())));
    }
}
