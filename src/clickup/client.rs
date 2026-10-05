use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

use crate::integrations::http::{client_builder, send_with_retry, RetryPolicy};

/// ClickUp's public REST API, v2. Overridable (see `ClickUpClient::new`)
/// only so tests can aim the client at a local mock server.
pub const DEFAULT_BASE_URL: &str = "https://api.clickup.com/api/v2";

/// Env var read through `AppState::env_source` to override the base URL
/// -- a test seam, never set in a real deployment.
#[cfg(test)]
pub const BASE_URL_ENV: &str = "CLICKUP_API_BASE_URL";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// ClickUp answers HTTP 401 for two completely different situations,
/// told apart only by `ECODE` (confirmed against the live API
/// 2026-10-02): `OAUTH_025` "Token invalid" (the user's token is bad),
/// and `OAUTH_027` "Team not authorized" -- *this token cannot see that
/// object*, which is also what a nonexistent list id returns. Treating
/// every 401 as a bad token would mark a perfectly good token invalid
/// the moment someone mistyped a list URL.
const ECODE_NOT_AUTHORIZED_FOR_OBJECT: &str = "OAUTH_027";

#[derive(Debug, thiserror::Error)]
pub enum ClickUpError {
    /// ClickUp rejected the token itself. Distinct from every other
    /// failure because it is the one a user can fix -- by pasting a
    /// valid token -- and the one that must flip a stored credential to
    /// "invalid".
    #[error("ClickUp rejected the API token")]
    Unauthorized,

    /// The object does not exist, or this token has no access to it
    /// (ClickUp does not distinguish the two). Says nothing about the
    /// token's validity.
    #[error("ClickUp has no such object, or this token cannot see it")]
    NotFound,

    /// ClickUp could not be reached at all (DNS, TLS, timeout). Says
    /// nothing about the token, so a stored credential must NOT be
    /// marked invalid on this.
    #[error("could not reach ClickUp: {0}")]
    Unreachable(#[from] reqwest::Error),

    #[error("ClickUp API returned {status}: {body}")]
    Api { status: u16, body: String },

    #[error("failed to parse ClickUp response ({0})")]
    Parse(serde_json::Error),
}

/// Who a ClickUp token belongs to, as ClickUp itself reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickUpIdentity {
    pub user_id: String,
    pub username: String,
    /// Names of the workspaces the token can see -- shown after a
    /// successful connect so the user can tell at a glance it is the
    /// right account.
    pub workspace_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ClickUpTeam {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ClickUpSpace {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ClickUpListRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ClickUpFolder {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub lists: Vec<ClickUpListRef>,
}

/// One list with its location, as `GET /list/{id}` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickUpListDetail {
    pub id: String,
    pub name: String,
    pub folder_id: Option<String>,
    pub folder_name: Option<String>,
    pub space_id: Option<String>,
}

#[derive(Deserialize)]
struct UserEnvelope {
    user: UserBody,
}

#[derive(Deserialize)]
struct UserBody {
    // ClickUp returns the id as a JSON number.
    id: serde_json::Number,
    username: String,
}

#[derive(Deserialize)]
struct TeamsEnvelope {
    teams: Vec<ClickUpTeam>,
}

#[derive(Deserialize)]
struct SpacesEnvelope {
    spaces: Vec<ClickUpSpace>,
}

#[derive(Deserialize)]
struct FoldersEnvelope {
    folders: Vec<ClickUpFolder>,
}

#[derive(Deserialize)]
struct NamedRef {
    id: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct ListEnvelope {
    id: String,
    name: String,
    folder: Option<NamedRef>,
    space: Option<NamedRef>,
}

#[derive(Deserialize)]
struct ViewEnvelope {
    view: ViewBody,
}

#[derive(Deserialize)]
struct ViewBody {
    parent: ViewParent,
}

#[derive(Deserialize)]
struct ViewParent {
    id: String,
    // 6 = list in ClickUp's parent-type numbering.
    #[serde(rename = "type")]
    kind: i64,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(rename = "ECODE")]
    ecode: Option<String>,
}

/// One connection pool for the whole process. `ClickUpClient::new` runs
/// on every handler call (`api::clickup_connection::clickup_client`), and
/// building a fresh `reqwest::Client` each time meant a new pool and a
/// new TLS handshake per request. `reqwest::Client` is a cheap
/// reference-counted handle, so cloning the shared one is free.
fn shared_http() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

    CLIENT
        .get_or_init(|| {
            client_builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("a reqwest client with only timeouts configured must build")
        })
        .clone()
}

/// Maps a ClickUp HTTP answer to its body or the matching [`ClickUpError`].
fn into_result(status: reqwest::StatusCode, body: String) -> Result<String, ClickUpError> {
    if status.is_success() {
        return Ok(body);
    }

    let ecode = serde_json::from_str::<ErrorBody>(&body)
        .ok()
        .and_then(|parsed| parsed.ecode);

    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(
            if ecode.as_deref() == Some(ECODE_NOT_AUTHORIZED_FOR_OBJECT) {
                ClickUpError::NotFound
            } else {
                ClickUpError::Unauthorized
            },
        );
    }

    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(ClickUpError::NotFound);
    }

    Err(ClickUpError::Api {
        status: status.as_u16(),
        body,
    })
}

pub struct ClickUpClient {
    // `pub(super)` so `tasks.rs` (the task-level calls) can build on them.
    pub(super) http: reqwest::Client,
    pub(super) base_url: String,
}

impl ClickUpClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            http: shared_http(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// GET `path` with `token` as the raw `Authorization` header value.
    /// ClickUp personal tokens are sent as-is -- **no `Bearer` prefix**
    /// (confirmed against the live API 2026-10-02). The token never
    /// appears in logs or in a returned error.
    pub(super) async fn get(&self, token: &str, path: &str) -> Result<String, ClickUpError> {
        // GET only, so repeating it on a transient failure is safe.
        let url = format!("{}{}", self.base_url, path);
        let response = send_with_retry(RetryPolicy::STANDARD, || {
            self.http
                .get(&url)
                .header(reqwest::header::AUTHORIZATION, token)
        })
        .await?;

        let status = response.status();
        let body = response.text().await?;
        into_result(status, body)
    }

    /// Sends `body` as JSON with `method`, **once and never retried**: a
    /// retried POST would post a second comment whenever the first
    /// request reached ClickUp but its answer was lost.
    pub(super) async fn send_json(
        &self,
        method: reqwest::Method,
        token: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<String, ClickUpError> {
        let response = self
            .http
            .request(method, format!("{}{}", self.base_url, path))
            .header(reqwest::header::AUTHORIZATION, token)
            .json(body)
            .send()
            .await?;

        let status = response.status();
        let text = response.text().await?;
        into_result(status, text)
    }

    pub(super) async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        token: &str,
        path: &str,
    ) -> Result<T, ClickUpError> {
        let body = self.get(token, path).await?;
        serde_json::from_str(&body).map_err(ClickUpError::Parse)
    }

    /// Confirms `token` works and reports whose it is. Two read-only
    /// calls: `GET /user` (identity) and `GET /team` (the workspaces it
    /// can see). Used both when a user first saves a token and when they
    /// re-test an existing one.
    pub async fn identify(&self, token: &str) -> Result<ClickUpIdentity, ClickUpError> {
        let user: UserEnvelope = self.get_json(token, "/user").await?;
        let teams = self.teams(token).await?;

        Ok(ClickUpIdentity {
            user_id: user.user.id.to_string(),
            username: user.user.username,
            workspace_names: teams.into_iter().map(|team| team.name).collect(),
        })
    }

    pub async fn teams(&self, token: &str) -> Result<Vec<ClickUpTeam>, ClickUpError> {
        Ok(self.get_json::<TeamsEnvelope>(token, "/team").await?.teams)
    }

    pub async fn spaces(
        &self,
        token: &str,
        team_id: &str,
    ) -> Result<Vec<ClickUpSpace>, ClickUpError> {
        Ok(self
            .get_json::<SpacesEnvelope>(token, &format!("/team/{team_id}/space?archived=false"))
            .await?
            .spaces)
    }

    /// Every non-archived folder in `space_id`, each with its lists --
    /// one call returns the whole onboarding hierarchy (292 folders /
    /// 816 lists at the time of writing).
    pub async fn folders(
        &self,
        token: &str,
        space_id: &str,
    ) -> Result<Vec<ClickUpFolder>, ClickUpError> {
        Ok(self
            .get_json::<FoldersEnvelope>(token, &format!("/space/{space_id}/folder?archived=false"))
            .await?
            .folders)
    }

    /// `GET /list/{id}`. A list that does not exist and one this token
    /// cannot see are indistinguishable to ClickUp, so both are
    /// `NotFound`.
    pub async fn list(
        &self,
        token: &str,
        list_id: &str,
    ) -> Result<ClickUpListDetail, ClickUpError> {
        let list: ListEnvelope = self.get_json(token, &format!("/list/{list_id}")).await?;

        let (folder_id, folder_name) = match list.folder {
            Some(folder) => (folder.id, folder.name),
            None => (None, None),
        };

        Ok(ClickUpListDetail {
            id: list.id,
            name: list.name,
            folder_id,
            folder_name,
            space_id: list.space.and_then(|space| space.id),
        })
    }

    /// The list a *view* belongs to -- the `80rbk-81594` in a pasted
    /// `.../v/l/80rbk-81594` URL is a view id, not a list id. `None`
    /// when the view hangs off something other than a list.
    pub async fn list_id_for_view(
        &self,
        token: &str,
        view_id: &str,
    ) -> Result<Option<String>, ClickUpError> {
        let view: ViewEnvelope = self.get_json(token, &format!("/view/{view_id}")).await?;
        Ok((view.view.parent.kind == 6).then_some(view.view.parent.id))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        extract::{Path, State},
        http::{HeaderMap, StatusCode},
        routing::get,
        Json, Router,
    };
    use serde_json::json;

    use super::*;

    /// Records every `Authorization` header the mock saw, so tests can
    /// assert the token went out raw (no `Bearer`).
    type SeenAuth = Arc<Mutex<Vec<String>>>;

    fn authorized(headers: &HeaderMap, seen: &SeenAuth) -> bool {
        let value = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let ok = value == "pk_good";
        seen.lock().unwrap().push(value);
        ok
    }

    fn token_invalid() -> (StatusCode, Json<serde_json::Value>) {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "err": "Token invalid", "ECODE": "OAUTH_025" })),
        )
    }

    type Reply = Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)>;

    async fn user_handler(State(seen): State<SeenAuth>, headers: HeaderMap) -> Reply {
        if !authorized(&headers, &seen) {
            return Err(token_invalid());
        }
        Ok(Json(
            json!({ "user": { "id": 106274085, "username": "Boris Maksimov" } }),
        ))
    }

    async fn team_handler(State(seen): State<SeenAuth>, headers: HeaderMap) -> Reply {
        if !authorized(&headers, &seen) {
            return Err(token_invalid());
        }
        Ok(Json(
            json!({ "teams": [ { "id": "8413555", "name": "QuikStor" } ] }),
        ))
    }

    async fn spaces_handler(
        State(seen): State<SeenAuth>,
        headers: HeaderMap,
        Path(_team): Path<String>,
    ) -> Reply {
        if !authorized(&headers, &seen) {
            return Err(token_invalid());
        }
        Ok(Json(
            json!({ "spaces": [ { "id": "1", "name": "Docs" }, { "id": "2", "name": "QMS Onboarding" } ] }),
        ))
    }

    async fn folders_handler(
        State(seen): State<SeenAuth>,
        headers: HeaderMap,
        Path(_space): Path<String>,
    ) -> Reply {
        if !authorized(&headers, &seen) {
            return Err(token_invalid());
        }
        Ok(Json(json!({ "folders": [
            { "id": "f1", "name": "Affordable Storage - Beau Ryan",
              "lists": [ { "id": "l1", "name": "Affordable Storage Synott" } ] },
            { "id": "f2", "name": "Empty Folder" }
        ] })))
    }

    async fn list_handler(
        State(seen): State<SeenAuth>,
        headers: HeaderMap,
        Path(id): Path<String>,
    ) -> Reply {
        if !authorized(&headers, &seen) {
            return Err(token_invalid());
        }
        if id == "l1" {
            return Ok(Json(json!({
                "id": "l1", "name": "Affordable Storage Synott",
                "folder": { "id": "f1", "name": "Affordable Storage - Beau Ryan", "hidden": false },
                "space": { "id": "2", "name": "QMS Onboarding" }
            })));
        }
        // What the real API does for an id that does not exist.
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "err": "Team not authorized", "ECODE": "OAUTH_027" })),
        ))
    }

    async fn view_handler(
        State(seen): State<SeenAuth>,
        headers: HeaderMap,
        Path(id): Path<String>,
    ) -> Reply {
        if !authorized(&headers, &seen) {
            return Err(token_invalid());
        }
        match id.as_str() {
            "80rbk-81594" => Ok(Json(
                json!({ "view": { "id": id, "parent": { "id": "l1", "type": 6 } } }),
            )),
            "folder-view" => Ok(Json(
                json!({ "view": { "id": id, "parent": { "id": "f1", "type": 5 } } }),
            )),
            _ => Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "err": "View not found", "ECODE": "ACCESS_118" })),
            )),
        }
    }

    async fn spawn_mock() -> (String, SeenAuth) {
        let seen: SeenAuth = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/user", get(user_handler))
            .route("/team", get(team_handler))
            .route("/team/{team}/space", get(spaces_handler))
            .route("/space/{space}/folder", get(folders_handler))
            .route("/list/{id}", get(list_handler))
            .route("/view/{id}", get(view_handler))
            .with_state(seen.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        (format!("http://{addr}"), seen)
    }

    #[tokio::test]
    async fn identify_returns_the_user_and_workspaces_and_sends_the_token_raw() {
        let (base_url, seen) = spawn_mock().await;
        let client = ClickUpClient::new(&base_url);

        let identity = client.identify("pk_good").await.expect("good token works");

        assert_eq!(identity.user_id, "106274085");
        assert_eq!(identity.username, "Boris Maksimov");
        assert_eq!(identity.workspace_names, vec!["QuikStor".to_string()]);
        // Both calls went out with the bare token -- no "Bearer ".
        assert_eq!(*seen.lock().unwrap(), vec!["pk_good", "pk_good"]);
    }

    #[tokio::test]
    async fn identify_reports_a_rejected_token_as_unauthorized() {
        let (base_url, _) = spawn_mock().await;
        let client = ClickUpClient::new(&base_url);

        let result = client.identify("pk_bad").await;

        assert!(matches!(result, Err(ClickUpError::Unauthorized)));
    }

    #[tokio::test]
    async fn identify_reports_an_unreachable_server_distinctly_from_a_bad_token() {
        // Port 1 on loopback: nothing listens, connection refused.
        let client = ClickUpClient::new("http://127.0.0.1:1");

        let result = client.identify("pk_good").await;

        assert!(matches!(result, Err(ClickUpError::Unreachable(_))));
    }

    #[tokio::test]
    async fn spaces_and_folders_parse_the_hierarchy() {
        let (base_url, _) = spawn_mock().await;
        let client = ClickUpClient::new(&base_url);

        let spaces = client.spaces("pk_good", "8413555").await.unwrap();
        assert_eq!(spaces[1].name, "QMS Onboarding");

        let folders = client.folders("pk_good", "2").await.unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0].lists[0].name, "Affordable Storage Synott");
        // A folder with no `lists` key parses as empty, not an error.
        assert!(folders[1].lists.is_empty());
    }

    #[tokio::test]
    async fn a_list_reports_its_folder_and_space() {
        let (base_url, _) = spawn_mock().await;
        let client = ClickUpClient::new(&base_url);

        let list = client.list("pk_good", "l1").await.unwrap();

        assert_eq!(list.name, "Affordable Storage Synott");
        assert_eq!(
            list.folder_name.as_deref(),
            Some("Affordable Storage - Beau Ryan")
        );
        assert_eq!(list.space_id.as_deref(), Some("2"));
    }

    #[tokio::test]
    async fn a_missing_list_is_not_found_not_a_bad_token() {
        let (base_url, _) = spawn_mock().await;
        let client = ClickUpClient::new(&base_url);

        // ClickUp answers 401 OAUTH_027 here; that must not be read as
        // "your token is invalid".
        let result = client.list("pk_good", "does-not-exist").await;

        assert!(matches!(result, Err(ClickUpError::NotFound)));
    }

    #[tokio::test]
    async fn a_bad_token_on_a_list_lookup_is_still_unauthorized() {
        let (base_url, _) = spawn_mock().await;
        let client = ClickUpClient::new(&base_url);

        let result = client.list("pk_bad", "l1").await;

        assert!(matches!(result, Err(ClickUpError::Unauthorized)));
    }

    #[tokio::test]
    async fn a_list_view_resolves_to_its_parent_list_and_other_views_do_not() {
        let (base_url, _) = spawn_mock().await;
        let client = ClickUpClient::new(&base_url);

        assert_eq!(
            client
                .list_id_for_view("pk_good", "80rbk-81594")
                .await
                .unwrap(),
            Some("l1".to_string())
        );
        assert_eq!(
            client
                .list_id_for_view("pk_good", "folder-view")
                .await
                .unwrap(),
            None
        );
        assert!(matches!(
            client.list_id_for_view("pk_good", "nope").await,
            Err(ClickUpError::NotFound)
        ));
    }

    #[test]
    fn a_trailing_slash_on_the_base_url_is_tolerated() {
        let client = ClickUpClient::new("http://example.test/api/v2/");
        assert_eq!(client.base_url, "http://example.test/api/v2");
    }
}
