use std::time::Duration;

use serde::Deserialize;

/// ClickUp's public REST API, v2. Overridable (see `ClickUpClient::new`)
/// only so tests can aim the client at a local mock server.
pub const DEFAULT_BASE_URL: &str = "https://api.clickup.com/api/v2";

/// Env var read through `AppState::env_source` to override the base URL
/// -- a test seam, never set in a real deployment.
#[cfg(test)]
pub const BASE_URL_ENV: &str = "CLICKUP_API_BASE_URL";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
pub enum ClickUpError {
    /// ClickUp rejected the token (HTTP 401). Distinct from every other
    /// failure because it is the one a user can fix -- by pasting a
    /// valid token -- and the one that must flip a stored credential to
    /// "invalid".
    #[error("ClickUp rejected the API token")]
    Unauthorized,

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
    teams: Vec<TeamBody>,
}

#[derive(Deserialize)]
struct TeamBody {
    name: String,
}

pub struct ClickUpClient {
    http: reqwest::Client,
    base_url: String,
}

impl ClickUpClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .expect("a reqwest client with only a timeout configured must build"),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// GET `path` with `token` as the raw `Authorization` header value.
    /// ClickUp personal tokens are sent as-is -- **no `Bearer` prefix**
    /// (confirmed against the live API 2026-10-02). The token never
    /// appears in logs or in a returned error.
    async fn get(&self, token: &str, path: &str) -> Result<String, ClickUpError> {
        let response = self
            .http
            .get(format!("{}{}", self.base_url, path))
            .header(reqwest::header::AUTHORIZATION, token)
            .send()
            .await?;

        let status = response.status();

        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(ClickUpError::Unauthorized);
        }

        let body = response.text().await?;

        if !status.is_success() {
            return Err(ClickUpError::Api {
                status: status.as_u16(),
                body,
            });
        }

        Ok(body)
    }

    /// Confirms `token` works and reports whose it is. Two read-only
    /// calls: `GET /user` (identity) and `GET /team` (the workspaces it
    /// can see). Used both when a user first saves a token and when they
    /// re-test an existing one.
    pub async fn identify(&self, token: &str) -> Result<ClickUpIdentity, ClickUpError> {
        let user_body = self.get(token, "/user").await?;
        let user: UserEnvelope = serde_json::from_str(&user_body).map_err(ClickUpError::Parse)?;

        let teams_body = self.get(token, "/team").await?;
        let teams: TeamsEnvelope =
            serde_json::from_str(&teams_body).map_err(ClickUpError::Parse)?;

        Ok(ClickUpIdentity {
            user_id: user.user.id.to_string(),
            username: user.user.username,
            workspace_names: teams.teams.into_iter().map(|team| team.name).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{extract::State, http::HeaderMap, routing::get, Json, Router};
    use serde_json::json;

    use super::*;

    /// Records every `Authorization` header the mock saw, so tests can
    /// assert the token went out raw (no `Bearer`).
    type SeenAuth = Arc<Mutex<Vec<String>>>;

    async fn record_auth(headers: &HeaderMap, seen: &SeenAuth) -> bool {
        let value = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let ok = value == "pk_good";
        seen.lock().unwrap().push(value);
        ok
    }

    async fn user_handler(
        State(seen): State<SeenAuth>,
        headers: HeaderMap,
    ) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
        if !record_auth(&headers, &seen).await {
            return Err(axum::http::StatusCode::UNAUTHORIZED);
        }
        Ok(Json(
            json!({ "user": { "id": 106274085, "username": "Boris Maksimov" } }),
        ))
    }

    async fn team_handler(
        State(seen): State<SeenAuth>,
        headers: HeaderMap,
    ) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
        if !record_auth(&headers, &seen).await {
            return Err(axum::http::StatusCode::UNAUTHORIZED);
        }
        Ok(Json(
            json!({ "teams": [ { "id": "8413555", "name": "QuikStor" } ] }),
        ))
    }

    async fn spawn_mock() -> (String, SeenAuth) {
        let seen: SeenAuth = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/user", get(user_handler))
            .route("/team", get(team_handler))
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

    #[test]
    fn a_trailing_slash_on_the_base_url_is_tolerated() {
        let client = ClickUpClient::new("http://example.test/api/v2/");
        assert_eq!(client.base_url, "http://example.test/api/v2");
    }
}
