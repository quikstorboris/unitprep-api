use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::dto::Entry;
use super::folders::pick_facility_folder;
use super::*;

/// A loopback Dropbox that records the Authorization header it saw and
/// answers the first request 503, later ones 200.
async fn spawn_flaky_dropbox() -> (String, Arc<AtomicUsize>, Arc<std::sync::Mutex<Vec<String>>>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen_auth = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let (counter, seen) = (calls.clone(), seen_auth.clone());

    let app = axum::Router::new().route(
        "/",
        axum::routing::post(move |headers: axum::http::HeaderMap| {
            let (counter, seen) = (counter.clone(), seen.clone());
            async move {
                let auth = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                seen.lock().unwrap().push(auth);

                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    axum::http::StatusCode::SERVICE_UNAVAILABLE
                } else {
                    axum::http::StatusCode::OK
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (url, calls, seen_auth)
}

#[tokio::test]
async fn send_authed_sends_the_cached_token_and_retries_a_transient_failure() {
    let (url, calls, seen_auth) = spawn_flaky_dropbox().await;
    let client = DropboxClient::new(DropboxConfig {
        app_key: "k".into(),
        app_secret: "s".into(),
        refresh_token: "r".into(),
        root_namespace_id: "1".into(),
        root_path: "/".into(),
    });
    // A live cached token, so no real OAuth refresh is attempted.
    *client.token.lock().await = Some(CachedToken {
        access_token: "cached-token".into(),
        expires_at: Instant::now() + Duration::from_secs(3600),
    });

    let response = client
        .send_authed(RetryPolicy::STANDARD, |token| {
            client.http.post(&url).bearer_auth(token)
        })
        .await
        .expect("the retry must succeed");

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "one 503, then the retry");
    assert_eq!(
        *seen_auth.lock().unwrap(),
        vec!["Bearer cached-token".to_string(); 2],
        "both attempts must carry the cached bearer token"
    );
}

// ---- a hermetic mock Dropbox (axum on loopback) -----------------------

use std::collections::VecDeque;

#[derive(Clone, Debug)]
struct Recorded {
    path: String,
    authorization: String,
    path_root: String,
    body: serde_json::Value,
}

#[derive(Default)]
struct MockDropbox {
    requests: std::sync::Mutex<Vec<Recorded>>,
    list_pages: std::sync::Mutex<VecDeque<serde_json::Value>>,
    /// Replies the mock gives BEFORE falling back to `list_pages`, as
    /// `(status, body)` -- for the non-listing calls and for error statuses.
    scripted: std::sync::Mutex<VecDeque<(u16, String)>>,
    /// Bearer tokens the mock answers with 401 (an expired/revoked token).
    reject_tokens: std::sync::Mutex<Vec<String>>,
    token_calls: AtomicUsize,
}

async fn mock_handler(
    axum::extract::State(mock): axum::extract::State<Arc<MockDropbox>>,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: String,
) -> (axum::http::StatusCode, String) {
    let path = uri.path().to_string();

    if path == "/oauth2/token" {
        let n = mock.token_calls.fetch_add(1, Ordering::SeqCst) + 1;
        return (
            axum::http::StatusCode::OK,
            serde_json::json!({ "access_token": format!("T{n}"), "expires_in": 14400 }).to_string(),
        );
    }

    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let authorization = header("authorization");
    mock.requests.lock().unwrap().push(Recorded {
        path,
        authorization: authorization.clone(),
        path_root: header("dropbox-api-path-root"),
        body: serde_json::from_str(&body).unwrap_or(serde_json::Value::Null),
    });

    let rejected = mock
        .reject_tokens
        .lock()
        .unwrap()
        .iter()
        .any(|t| authorization == format!("Bearer {t}"));
    if rejected {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            r#"{"error_summary": "expired_access_token/"}"#.to_string(),
        );
    }

    if let Some((status, body)) = mock.scripted.lock().unwrap().pop_front() {
        return (axum::http::StatusCode::from_u16(status).unwrap(), body);
    }

    match mock.list_pages.lock().unwrap().pop_front() {
        Some(page) => (axum::http::StatusCode::OK, page.to_string()),
        None => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "mock ran out of pages".to_string(),
        ),
    }
}

async fn spawn_mock(mock: Arc<MockDropbox>) -> DropboxClient {
    let app = axum::Router::new().fallback(mock_handler).with_state(mock);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    DropboxClient::with_endpoints(
        DropboxConfig {
            app_key: "k".into(),
            app_secret: "s".into(),
            refresh_token: "r".into(),
            root_namespace_id: "ns1".into(),
            root_path: "/Root".into(),
        },
        base.clone(),
        base,
    )
}

fn entry(name: &str) -> serde_json::Value {
    serde_json::json!({ ".tag": "folder", "name": name, "path_display": format!("/Root/{name}") })
}

fn page(names: &[&str], has_more: bool, cursor: &str) -> serde_json::Value {
    serde_json::json!({
        "entries": names.iter().map(|n| entry(n)).collect::<Vec<_>>(),
        "has_more": has_more,
        "cursor": cursor,
    })
}

#[tokio::test]
async fn list_folder_follows_pagination_until_has_more_is_false() {
    let mock = Arc::new(MockDropbox::default());
    mock.list_pages.lock().unwrap().extend([
        page(&["a", "b"], true, "c1"),
        page(&["c"], true, "c2"),
        page(&["d"], false, "c3"),
    ]);
    let client = spawn_mock(mock.clone()).await;

    let entries = client.list_folder("/Root").await.unwrap();

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b", "c", "d"], "every page, in order");

    let requests = mock.requests.lock().unwrap().clone();
    let paths: Vec<&str> = requests.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "/2/files/list_folder",
            "/2/files/list_folder/continue",
            "/2/files/list_folder/continue"
        ]
    );
    assert_eq!(requests[0].body["path"], "/Root");
    assert_eq!(requests[0].body["recursive"], false);
    assert_eq!(requests[1].body["cursor"], "c1");
    assert_eq!(requests[2].body["cursor"], "c2");
    assert!(
        requests.iter().all(|r| r.path_root.contains("ns1")),
        "every page must carry the namespace root header"
    );
}

#[tokio::test]
async fn list_folder_with_one_page_makes_exactly_one_request() {
    let mock = Arc::new(MockDropbox::default());
    mock.list_pages
        .lock()
        .unwrap()
        .push_back(page(&["only"], false, "c1"));
    let client = spawn_mock(mock.clone()).await;

    let entries = client.list_folder("/Root").await.unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(mock.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_401_refreshes_the_token_and_retries_once_with_the_fresh_one() {
    let mock = Arc::new(MockDropbox::default());
    // The first token the mock mints (T1) is "expired" by the time it is used.
    mock.reject_tokens.lock().unwrap().push("T1".to_string());
    mock.list_pages
        .lock()
        .unwrap()
        .push_back(page(&["x"], false, "c"));
    let client = spawn_mock(mock.clone()).await;

    let entries = client.list_folder("/Root").await.unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(
        mock.token_calls.load(Ordering::SeqCst),
        2,
        "one refresh to get T1, one more after its 401"
    );
    let auths: Vec<String> = mock
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.authorization.clone())
        .collect();
    assert_eq!(auths, vec!["Bearer T1", "Bearer T2"]);
}

#[tokio::test]
async fn a_second_401_is_returned_as_an_error_not_retried_forever() {
    let mock = Arc::new(MockDropbox::default());
    mock.reject_tokens.lock().unwrap().extend([
        "T1".to_string(),
        "T2".to_string(),
        "T3".to_string(),
    ]);
    let client = spawn_mock(mock.clone()).await;

    let err = client.list_folder("/Root").await.unwrap_err();

    assert!(matches!(err, DropboxError::Api { status: 401, .. }));
    assert_eq!(
        mock.token_calls.load(Ordering::SeqCst),
        2,
        "exactly one refresh-and-retry, then give up"
    );
    assert_eq!(mock.requests.lock().unwrap().len(), 2);
}

#[test]
fn picks_the_exact_name_match_over_any_other_candidate() {
    let folders = vec![
        Entry::test_folder("Sand-Sto Storage", "/qms onboarding/sand-sto storage"),
        Entry::test_folder(
            "Sand-Sto Climate Controlled Storage",
            "/qms onboarding/sand-sto climate controlled storage",
        ),
    ];

    let picked = pick_facility_folder(folders, "Sand-Sto Climate Controlled Storage")
        .expect("an exact match exists and must be picked");

    assert_eq!(picked.name, "Sand-Sto Climate Controlled Storage");
}

// The real Sand-Sto case (2026-09-04): OO's own facility name
// ("Sand-Sto Climate Controlled Storage") doesn't match the real
// Dropbox folder someone actually created for it ("Sand-Sto
// Storage") -- a single non-exact candidate must NOT be silently
// picked (see `pick_facility_folder`'s own doc comment for why a
// same-day fallback attempt here was reverted: Dropbox's search
// returned exactly one result for this exact real query and it was
// an unrelated folder, not this one).
#[test]
fn picks_nothing_when_a_single_candidate_does_not_match_exactly() {
    let folders = vec![Entry::test_folder(
        "Sand-Sto Storage",
        "/qms onboarding/sand-sto storage",
    )];

    assert!(pick_facility_folder(folders, "Sand-Sto Climate Controlled Storage").is_none());
}

#[test]
fn picks_nothing_when_multiple_candidates_have_no_exact_match() {
    let folders = vec![
        Entry::test_folder("Sand-Sto Storage", "/qms onboarding/sand-sto storage"),
        Entry::test_folder(
            "Sand-Sto Self Storage",
            "/qms onboarding/sand-sto self storage",
        ),
    ];

    assert!(pick_facility_folder(folders, "Sand-Sto Climate Controlled Storage").is_none());
}

#[test]
fn picks_nothing_when_search_returns_no_candidates_at_all() {
    assert!(pick_facility_folder(vec![], "Nonexistent Facility").is_none());
}

fn script(mock: &MockDropbox, replies: &[(u16, serde_json::Value)]) {
    mock.scripted.lock().unwrap().extend(
        replies
            .iter()
            .map(|(status, body)| (*status, body.to_string())),
    );
}

#[tokio::test]
async fn search_folders_keeps_only_folders_and_scopes_to_the_root() {
    let mock = Arc::new(MockDropbox::default());
    script(
        &mock,
        &[(
            200,
            serde_json::json!({ "matches": [
                { "metadata": { "metadata": entry("Highway 20") } },
                { "metadata": { "metadata": {
                    ".tag": "file", "name": "Highway 20.csv", "path_display": "/Root/Highway 20.csv"
                } } },
            ] }),
        )],
    );
    let client = spawn_mock(mock.clone()).await;

    let folders = client.search_folders("highway").await.unwrap();

    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].name, "Highway 20");
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(requests[0].path, "/2/files/search_v2");
    assert_eq!(requests[0].body["query"], "highway");
    assert_eq!(requests[0].body["options"]["path"], "/Root");
    assert!(requests[0].path_root.contains("ns1"));
}

#[tokio::test]
async fn a_failed_listing_surfaces_the_status_and_body() {
    let mock = Arc::new(MockDropbox::default());
    script(
        &mock,
        &[(
            409,
            serde_json::json!({ "error_summary": "path/not_found/" }),
        )],
    );
    let client = spawn_mock(mock).await;

    let err = client.list_folder("/Root/missing").await.unwrap_err();

    assert!(
        matches!(&err, DropboxError::Api { status: 409, body } if body.contains("path/not_found")),
        "got {err:?}"
    );
}

#[tokio::test]
async fn resolve_shared_link_bridges_the_link_id_to_a_real_path() {
    let mock = Arc::new(MockDropbox::default());
    script(
        &mock,
        &[
            (200, serde_json::json!({ ".tag": "folder", "id": "id:abc" })),
            (200, entry("Sand-Sto Storage")),
        ],
    );
    let client = spawn_mock(mock.clone()).await;

    let resolved = client
        .resolve_shared_link("https://www.dropbox.com/scl/fo/x")
        .await
        .unwrap();

    assert_eq!(resolved.unwrap().path_display, "/Root/Sand-Sto Storage");
    let requests = mock.requests.lock().unwrap().clone();
    assert_eq!(requests[0].path, "/2/sharing/get_shared_link_metadata");
    assert_eq!(
        requests[0].path_root, "",
        "the link lookup is not namespace-relative"
    );
    assert_eq!(requests[1].path, "/2/files/get_metadata");
    assert_eq!(requests[1].body["path"], "id:abc");
    assert!(requests[1].path_root.contains("ns1"));
}

#[tokio::test]
async fn resolve_shared_link_degrades_to_none_for_a_file_link_or_a_failure() {
    let mock = Arc::new(MockDropbox::default());
    script(
        &mock,
        &[
            (200, serde_json::json!({ ".tag": "file", "id": "id:f" })),
            (
                404,
                serde_json::json!({ "error_summary": "shared_link_not_found/" }),
            ),
        ],
    );
    let client = spawn_mock(mock).await;

    assert!(client.resolve_shared_link("u1").await.unwrap().is_none());
    assert!(client.resolve_shared_link("u2").await.unwrap().is_none());
}

#[tokio::test]
async fn create_folder_if_missing_treats_an_existing_folder_as_success() {
    let mock = Arc::new(MockDropbox::default());
    script(
        &mock,
        &[
            (200, serde_json::json!({})),
            (
                409,
                serde_json::json!({ "error_summary": "path/conflict/folder/..." }),
            ),
            (
                409,
                serde_json::json!({ "error_summary": "path/insufficient_space/" }),
            ),
        ],
    );
    let client = spawn_mock(mock).await;

    client.create_folder_if_missing("/Root/a").await.unwrap();
    client.create_folder_if_missing("/Root/a").await.unwrap();
    let err = client
        .create_folder_if_missing("/Root/b")
        .await
        .unwrap_err();

    assert!(
        matches!(err, DropboxError::Api { status: 409, .. }),
        "got {err:?}"
    );
}

#[tokio::test]
async fn shared_link_returns_the_new_link_or_falls_back_to_the_existing_one() {
    let mock = Arc::new(MockDropbox::default());
    script(
        &mock,
        &[
            (
                200,
                serde_json::json!({ "url": "https://dropbox.test/new" }),
            ),
            (
                409,
                serde_json::json!({ "error_summary": "shared_link_already_exists/" }),
            ),
            (
                200,
                serde_json::json!({ "links": [{ "url": "https://dropbox.test/old" }] }),
            ),
        ],
    );
    let client = spawn_mock(mock.clone()).await;

    assert_eq!(
        client.shared_link("/Root/f.csv").await.unwrap(),
        "https://dropbox.test/new"
    );
    assert_eq!(
        client.shared_link("/Root/f.csv").await.unwrap(),
        "https://dropbox.test/old"
    );

    let paths: Vec<String> = mock
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|r| r.path.clone())
        .collect();
    assert_eq!(
        paths,
        vec![
            "/2/sharing/create_shared_link_with_settings",
            "/2/sharing/create_shared_link_with_settings",
            "/2/sharing/list_shared_links",
        ]
    );
}
