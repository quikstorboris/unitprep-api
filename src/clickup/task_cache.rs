//! A short-lived cache of a list's tasks, per user and list. Reading a
//! whole onboarding list costs ClickUp round trips (and a big payload), and
//! the duplicate-check panel asks for it each time it opens -- and a
//! second time whenever the page re-renders its first request away. Kept
//! brief (5 minutes; the candidates show status and assignees, and the
//! update itself re-reads the chosen task) and dropped for a list
//! the moment Orchestrator writes to it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::tasks::ClickUpTask;

const TTL: Duration = Duration::from_secs(300);

type Cache = Mutex<HashMap<(Uuid, String), (Instant, Arc<Vec<ClickUpTask>>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

type Loads = Mutex<HashMap<(Uuid, String), Arc<tokio::sync::Mutex<()>>>>;

/// The cached tasks, or `load`'s result (which is then cached). Requests
/// for the same list that arrive together wait for one load rather than
/// each reading the whole list from ClickUp.
pub async fn get_or_load<F, Fut, E>(
    user_id: Uuid,
    list_id: &str,
    load: F,
) -> Result<Arc<Vec<ClickUpTask>>, E>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Vec<ClickUpTask>, E>>,
{
    if let Some(tasks) = get(user_id, list_id) {
        return Ok(tasks);
    }

    static LOADS: OnceLock<Loads> = OnceLock::new();
    let lock = LOADS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("task load lock map")
        .entry((user_id, list_id.to_string()))
        .or_default()
        .clone();
    let _guard = lock.lock().await;

    if let Some(tasks) = get(user_id, list_id) {
        return Ok(tasks);
    }

    Ok(put(user_id, list_id, load().await?))
}

pub fn get(user_id: Uuid, list_id: &str) -> Option<Arc<Vec<ClickUpTask>>> {
    let cache = cache().lock().expect("task cache lock");
    let (fetched_at, tasks) = cache.get(&(user_id, list_id.to_string()))?;
    (fetched_at.elapsed() < TTL).then(|| tasks.clone())
}

pub fn put(user_id: Uuid, list_id: &str, tasks: Vec<ClickUpTask>) -> Arc<Vec<ClickUpTask>> {
    let tasks = Arc::new(tasks);
    let mut cache = cache().lock().expect("task cache lock");
    // Bound the map: drop anything expired before adding.
    cache.retain(|_, (fetched_at, _)| fetched_at.elapsed() < TTL);
    cache.insert(
        (user_id, list_id.to_string()),
        (Instant::now(), tasks.clone()),
    );
    tasks
}

/// Forgets every user's cached copy of `list_id` (called after a write to
/// one of its tasks).
pub fn invalidate_list(list_id: &str) {
    cache()
        .lock()
        .expect("task cache lock")
        .retain(|(_, cached_list), _| cached_list != list_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cached_list_is_served_until_invalidated() {
        let user = Uuid::new_v4();
        assert!(get(user, "L9").is_none());

        put(user, "L9", Vec::new());
        assert!(get(user, "L9").is_some());
        assert!(get(Uuid::new_v4(), "L9").is_none());

        invalidate_list("L9");
        assert!(get(user, "L9").is_none());
    }
}
