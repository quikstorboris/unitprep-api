//! The ClickUp onboarding hierarchy (folders -> lists) for the one space
//! Orchestrator links against, fetched with the *acting user's* token.
//!
//! ClickUp has no useful "search lists by name" call, but the whole space
//! comes back in one request (292 folders / 816 lists at the time of
//! writing), so we fetch it once, cache it briefly per user, and do all
//! the fuzzy matching locally -- instant, and no per-keystroke traffic
//! against ClickUp's rate limit. The cache is per user because each
//! user's token may see a different slice of the workspace.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::client::{ClickUpClient, ClickUpError, ClickUpFolder, ClickUpListRef};
use super::matching::{is_facility_list, ListEntry};

/// How long a fetched hierarchy is reused. Short: a list created in
/// ClickUp a minute ago should show up the next time someone opens the
/// Link dialog.
const CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, thiserror::Error)]
pub enum HierarchyError {
    #[error(transparent)]
    ClickUp(#[from] ClickUpError),

    #[error("No ClickUp space named \"{0}\" is visible to this token.")]
    SpaceNotFound(String),
}

#[derive(Debug, Clone)]
pub struct Hierarchy {
    pub team_id: String,
    pub space_id: String,
    pub folders: Vec<ClickUpFolder>,
}

impl Hierarchy {
    /// The URL that opens `list_id` in the ClickUp web app.
    pub fn list_url(&self, list_id: &str) -> String {
        list_url(&self.team_id, list_id)
    }

    /// The folder and list for `list_id` when it is one of this space's
    /// foldered lists -- lets a caller confirm a list from the data it
    /// already holds instead of asking ClickUp again. A folderless list
    /// is not here (the hierarchy only walks folders).
    pub fn find_list(&self, list_id: &str) -> Option<(&ClickUpFolder, &ClickUpListRef)> {
        self.folders.iter().find_map(|folder| {
            folder
                .lists
                .iter()
                .find(|list| list.id == list_id)
                .map(|list| (folder, list))
        })
    }

    /// Every list that is an actual facility's onboarding list (see
    /// [`is_facility_list`]) as a match candidate.
    pub fn facility_lists(&self) -> Vec<ListEntry> {
        self.folders
            .iter()
            .flat_map(|folder| {
                folder
                    .lists
                    .iter()
                    .filter(|list| is_facility_list(&list.name))
                    .map(|list| ListEntry {
                        list_id: list.id.clone(),
                        list_name: list.name.trim().to_string(),
                        folder_id: folder.id.clone(),
                        folder_name: folder.name.trim().to_string(),
                    })
            })
            .collect()
    }
}

pub fn list_url(team_id: &str, list_id: &str) -> String {
    format!("https://app.clickup.com/{team_id}/v/li/{list_id}")
}

/// Fetches the named space's folders and lists. The workspace is found
/// by looking for the space by name across the token's workspaces, so
/// nothing about QuikStor's ClickUp ids is hardcoded here.
pub async fn load(
    client: &ClickUpClient,
    token: &str,
    space_name: &str,
) -> Result<Hierarchy, HierarchyError> {
    for team in client.teams(token).await? {
        let spaces = client.spaces(token, &team.id).await?;

        if let Some(space) = spaces
            .into_iter()
            .find(|space| space.name.eq_ignore_ascii_case(space_name))
        {
            let folders = client.folders(token, &space.id).await?;
            return Ok(Hierarchy {
                team_id: team.id,
                space_id: space.id,
                folders,
            });
        }
    }

    Err(HierarchyError::SpaceNotFound(space_name.to_string()))
}

type Cache = Mutex<HashMap<Uuid, (Instant, Arc<Hierarchy>)>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fresh(user_id: Uuid) -> Option<Arc<Hierarchy>> {
    let cache = cache().lock().expect("cache lock");
    let (fetched_at, hierarchy) = cache.get(&user_id)?;
    (fetched_at.elapsed() < CACHE_TTL).then(|| hierarchy.clone())
}

type Loads = Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>;

/// One lock per user, so requests that arrive together on a cold cache
/// (the Link dialog fires its list catalog and its suggestions at once)
/// share a single three-call ClickUp walk instead of each making their own.
fn load_lock(user_id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
    static LOADS: OnceLock<Loads> = OnceLock::new();
    LOADS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("load lock map")
        .entry(user_id)
        .or_default()
        .clone()
}

/// The cached hierarchy for `user_id` if it is fresh, otherwise a newly
/// fetched one (which is then cached).
pub async fn cached_or_load(
    user_id: Uuid,
    client: &ClickUpClient,
    token: &str,
    space_name: &str,
) -> Result<Arc<Hierarchy>, HierarchyError> {
    if let Some(hierarchy) = fresh(user_id) {
        return Ok(hierarchy);
    }

    let lock = load_lock(user_id);
    let _guard = lock.lock().await;

    // Whoever held the lock before us may have just loaded it.
    if let Some(hierarchy) = fresh(user_id) {
        return Ok(hierarchy);
    }

    let hierarchy = Arc::new(load(client, token, space_name).await?);
    cache()
        .lock()
        .expect("cache lock")
        .insert(user_id, (Instant::now(), hierarchy.clone()));
    Ok(hierarchy)
}

/// Drops `user_id`'s cached hierarchy -- called when their token is
/// replaced or removed, so a stale view of what the *old* token could
/// see is never served to the new one.
pub fn invalidate(user_id: Uuid) {
    cache().lock().expect("cache lock").remove(&user_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clickup::client::ClickUpListRef;

    fn folder(id: &str, name: &str, lists: &[(&str, &str)]) -> ClickUpFolder {
        ClickUpFolder {
            id: id.to_string(),
            name: name.to_string(),
            lists: lists
                .iter()
                .map(|(id, name)| ClickUpListRef {
                    id: id.to_string(),
                    name: name.to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn list_urls_open_the_list_in_the_workspace() {
        let hierarchy = Hierarchy {
            team_id: "8413555".to_string(),
            space_id: "s".to_string(),
            folders: Vec::new(),
        };

        assert_eq!(
            hierarchy.list_url("901418685125"),
            "https://app.clickup.com/8413555/v/li/901418685125"
        );
    }

    #[test]
    fn only_facility_lists_become_candidates_with_trimmed_names() {
        let hierarchy = Hierarchy {
            team_id: "t".to_string(),
            space_id: "s".to_string(),
            folders: vec![folder(
                "f1",
                " Affordable Self Storage",
                &[
                    ("l1", "Affordable Self Storage"),
                    ("l2", "Post-Onboarding - Affordable"),
                    ("l3", "🎈 Affordable Template"),
                    ("l4", "Key West Mini Storage "),
                ],
            )],
        };

        let names: Vec<String> = hierarchy
            .facility_lists()
            .into_iter()
            .map(|entry| entry.list_name)
            .collect();

        assert_eq!(
            names,
            vec!["Affordable Self Storage", "Key West Mini Storage"]
        );
        assert_eq!(
            hierarchy.facility_lists()[0].folder_name,
            "Affordable Self Storage"
        );
    }

    #[test]
    fn invalidating_a_user_with_no_cache_entry_is_harmless() {
        invalidate(Uuid::new_v4());
    }
}
