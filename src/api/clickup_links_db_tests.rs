//! Real-database tests for linking facilities to ClickUp lists: the
//! suggestion/resolve/save/unlink endpoints end to end against a mock
//! ClickUp and the local ephemeral `test-db` (as `app_service`, so RLS
//! genuinely applies). Every test is `#[ignore]`d -- see
//! `clickup_db_tests`' module doc for how to run them.
//!
//! The database is shared and never wiped between tests, so every
//! fixture gets **its own random list ids** (see [`Ids`]): a fixed id
//! would collide with links left behind by an earlier test and make the
//! "already linked elsewhere" checks flaky.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::extract::{ConnectInfo, Json, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::clickup_db_tests::{body_json, caller, create_user, superuser_pool};
use crate::api::test_support::{empty_state, FakeEnvSource};
use crate::api::{clickup_connection, clickup_lookup, clients_clickup_links, AppState};
use crate::auth::AuthenticatedUser;

const ENCRYPTION_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn local_addr() -> ConnectInfo<SocketAddr> {
    ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0)))
}

/// ClickUp list ids (numeric, like the real ones) unique to one fixture.
#[derive(Clone)]
struct Ids {
    synott: String,
    copperfield: String,
    post_onboarding: String,
    other_space: String,
}

impl Ids {
    fn unique() -> Self {
        let n = (Uuid::new_v4().as_u128() % 900_000_000) as u64 + 100_000_000;
        Self {
            synott: format!("{n}1"),
            copperfield: format!("{n}2"),
            post_onboarding: format!("{n}3"),
            other_space: format!("{n}4"),
        }
    }
}

type Reply = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn bad_token() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "err": "Token invalid", "ECODE": "OAUTH_025" })),
    )
}

fn no_access() -> (StatusCode, Json<Value>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "err": "Team not authorized", "ECODE": "OAUTH_027" })),
    )
}

fn token_ok(headers: &HeaderMap, accepting: &AtomicBool) -> bool {
    accepting.load(Ordering::SeqCst)
        && headers.get("authorization").and_then(|v| v.to_str().ok()) == Some("pk_good")
}

/// A mock ClickUp: workspace 8413555 with a "QMS Onboarding" space (id 2)
/// holding two Affordable Storage facility lists and a Post-Onboarding
/// list, plus a list that lives in a different space. Accepts only the
/// token "pk_good", and only while `accepting`.
async fn spawn_clickup(accepting: Arc<AtomicBool>, ids: Ids) -> String {
    let (a_user, a_team, a_space, a_folder, a_list, a_view) = (
        accepting.clone(),
        accepting.clone(),
        accepting.clone(),
        accepting.clone(),
        accepting.clone(),
        accepting,
    );
    let (ids_folder, ids_list, ids_view) = (ids.clone(), ids.clone(), ids);

    let app = Router::new()
        .route(
            "/user",
            get(move |h: HeaderMap| {
                let a = a_user.clone();
                async move {
                    if !token_ok(&h, &a) {
                        return Err(bad_token());
                    }
                    Ok(Json(json!({ "user": { "id": 42, "username": "Test Person" } }))) as Reply
                }
            }),
        )
        .route(
            "/team",
            get(move |h: HeaderMap| {
                let a = a_team.clone();
                async move {
                    if !token_ok(&h, &a) {
                        return Err(bad_token());
                    }
                    Ok(Json(json!({ "teams": [ { "id": "8413555", "name": "QuikStor" } ] }))) as Reply
                }
            }),
        )
        .route(
            "/team/{team}/space",
            get(move |h: HeaderMap| {
                let a = a_space.clone();
                async move {
                    if !token_ok(&h, &a) {
                        return Err(bad_token());
                    }
                    Ok(Json(json!({ "spaces": [
                        { "id": "1", "name": "Documentation" },
                        { "id": "2", "name": "QMS Onboarding" }
                    ] }))) as Reply
                }
            }),
        )
        .route(
            "/space/{space}/folder",
            get(move |h: HeaderMap| {
                let a = a_folder.clone();
                let ids = ids_folder.clone();
                async move {
                    if !token_ok(&h, &a) {
                        return Err(bad_token());
                    }
                    Ok(Json(json!({ "folders": [
                        { "id": "f1", "name": "Affordable Storage - Beau Ryan", "lists": [
                            { "id": ids.synott, "name": "Affordable Storage Synott" },
                            { "id": ids.copperfield, "name": "Affordable Storage Copperfield" },
                            { "id": ids.post_onboarding, "name": "Post-Onboarding - Affordable Storage" }
                        ] }
                    ] }))) as Reply
                }
            }),
        )
        .route(
            "/list/{id}",
            get(move |h: HeaderMap, Path(id): Path<String>| {
                let a = a_list.clone();
                let ids = ids_list.clone();
                async move {
                    if !token_ok(&h, &a) {
                        return Err(bad_token());
                    }
                    let (name, folder, space) = if id == ids.synott {
                        ("Affordable Storage Synott", "Affordable Storage - Beau Ryan", "2")
                    } else if id == ids.copperfield {
                        ("Affordable Storage Copperfield", "Affordable Storage - Beau Ryan", "2")
                    } else if id == ids.other_space {
                        // Exists, but in a different space.
                        ("Some Documentation List", "Docs Folder", "1")
                    } else {
                        return Err(no_access());
                    };
                    Ok(Json(json!({
                        "id": id, "name": name,
                        "folder": { "id": "f1", "name": folder },
                        "space": { "id": space, "name": "x" }
                    }))) as Reply
                }
            }),
        )
        .route(
            "/view/{id}",
            get(move |h: HeaderMap, Path(id): Path<String>| {
                let a = a_view.clone();
                let ids = ids_view.clone();
                async move {
                    if !token_ok(&h, &a) {
                        return Err(bad_token());
                    }
                    match id.as_str() {
                        "80rbk-1" => Ok(Json(json!({ "view": { "parent": { "id": ids.synott, "type": 6 } } }))) as Reply,
                        "folder-1" => Ok(Json(json!({ "view": { "parent": { "id": "f1", "type": 5 } } }))),
                        _ => Err((
                            StatusCode::NOT_FOUND,
                            Json(json!({ "err": "View not found", "ECODE": "ACCESS_118" })),
                        )),
                    }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

struct Fixture {
    state: AppState,
    superuser: PgPool,
    ids: Ids,
    user_id: Uuid,
    company_id: Uuid,
    synott: Uuid,
    copperfield: Uuid,
}

impl Fixture {
    /// A user with a saved, valid ClickUp token, and a company with two
    /// Affordable Storage facilities.
    async fn new(accepting: Arc<AtomicBool>) -> Self {
        std::env::set_var("INTEGRATION_SECRETS_ENCRYPTION_KEY", ENCRYPTION_KEY);
        let ids = Ids::unique();
        let base_url = spawn_clickup(accepting, ids.clone()).await;
        let superuser = superuser_pool();
        let user_id = create_user(&superuser, "linker").await;

        let state = AppState {
            db: crate::db::connect_test(),
            env_source: Arc::new(FakeEnvSource::with(&[("CLICKUP_API_BASE_URL", &base_url)])),
            ..empty_state()
        };

        // Connect the user's ClickUp token through the real endpoint.
        let saved = clickup_connection::save_token(
            State(state.clone()),
            Self::clickup_caller_for(user_id, &["onboarding_manager"]),
            local_addr(),
            HeaderMap::new(),
            Json(clickup_connection::SaveTokenRequest {
                token: "pk_good".to_string(),
            }),
        )
        .await;
        assert_eq!(saved.status(), StatusCode::OK);

        let company_id: Uuid = sqlx::query_scalar(
            "INSERT INTO clients.companies (legal_name, source) VALUES ('Affordable Storage', 'manual') RETURNING id",
        )
        .fetch_one(&superuser)
        .await
        .unwrap();

        let mut facility_ids = Vec::new();
        for (name, city) in [
            ("Affordable Storage Synott", "Houston"),
            ("Affordable Storage Copperfield", "Houston"),
        ] {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO clients.facilities (company_id, name, city, source) VALUES ($1, $2, $3, 'manual') RETURNING id",
            )
            .bind(company_id)
            .bind(name)
            .bind(city)
            .fetch_one(&superuser)
            .await
            .unwrap();
            facility_ids.push(id);
        }

        Self {
            state,
            superuser,
            ids,
            user_id,
            company_id,
            synott: facility_ids[0],
            copperfield: facility_ids[1],
        }
    }

    fn clickup_caller_for(user_id: Uuid, roles: &[&str]) -> AuthenticatedUser {
        caller(user_id, roles, &["integrations.clickup"])
    }

    /// The ClickUp user with a client-ops role (may write facilities).
    fn manager(&self) -> AuthenticatedUser {
        Self::clickup_caller_for(self.user_id, &["onboarding_manager"])
    }

    /// Holds the ClickUp permission but no client-ops role.
    fn no_client_ops_role(&self) -> AuthenticatedUser {
        Self::clickup_caller_for(self.user_id, &[])
    }

    async fn link_columns(
        &self,
        facility: Uuid,
    ) -> (Option<String>, Option<String>, Option<String>) {
        sqlx::query_as(
            "SELECT clickup_list_id, clickup_list_name, clickup_list_url FROM clients.facilities WHERE id = $1",
        )
        .bind(facility)
        .fetch_one(&self.superuser)
        .await
        .unwrap()
    }

    async fn save(
        &self,
        who: AuthenticatedUser,
        links: &[(Uuid, &str)],
    ) -> axum::response::Response {
        clients_clickup_links::save_clickup_links(
            State(self.state.clone()),
            who,
            HeaderMap::new(),
            Path(self.company_id),
            Json(clients_clickup_links::SaveLinksRequest {
                links: links
                    .iter()
                    .map(
                        |(facility_id, list_id)| clients_clickup_links::LinkRequest {
                            facility_id: *facility_id,
                            list_id: list_id.to_string(),
                        },
                    )
                    .collect(),
            }),
        )
        .await
    }

    async fn resolve(&self, url: &str) -> axum::response::Response {
        clickup_lookup::resolve_clickup_url(
            State(self.state.clone()),
            self.manager(),
            Json(clickup_lookup::ResolveUrlRequest {
                url: url.to_string(),
            }),
        )
        .await
    }

    async fn unlink_one(&self, who: AuthenticatedUser, facility: Uuid) -> axum::response::Response {
        clients_clickup_links::unlink_facility_clickup(
            State(self.state.clone()),
            who,
            HeaderMap::new(),
            Path((self.company_id, facility)),
        )
        .await
    }
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_links_db_suggestions_pair_each_facility_with_its_own_list_and_hide_non_facility_lists(
) {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new(Arc::new(AtomicBool::new(true))).await;

    // The dropdown catalog excludes the Post-Onboarding list.
    let lists = clickup_lookup::list_clickup_lists(State(fx.state.clone()), fx.manager()).await;
    assert_eq!(lists.status(), StatusCode::OK);
    let lists = body_json(lists).await;
    let names: Vec<&str> = lists["lists"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["list_name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "Affordable Storage Copperfield",
            "Affordable Storage Synott"
        ]
    );
    assert_eq!(
        lists["lists"][0]["url"],
        format!(
            "https://app.clickup.com/8413555/v/li/{}",
            fx.ids.copperfield
        )
    );

    // Each facility is suggested its own list, never the same one twice.
    let suggestions = clickup_lookup::clickup_suggestions(
        State(fx.state.clone()),
        fx.manager(),
        Path(fx.company_id),
    )
    .await;
    assert_eq!(suggestions.status(), StatusCode::OK);
    let suggestions = body_json(suggestions).await;

    let by_name = |name: &str| -> Value {
        suggestions["facilities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["facility_name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(
        by_name("Affordable Storage Synott")["suggestion"]["list"]["list_id"],
        fx.ids.synott
    );
    assert_eq!(
        by_name("Affordable Storage Copperfield")["suggestion"]["list"]["list_id"],
        fx.ids.copperfield
    );
    assert!(by_name("Affordable Storage Synott")["current"].is_null());

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_links_db_a_pasted_url_resolves_with_the_lists_real_name_and_bad_ones_are_explained(
) {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new(Arc::new(AtomicBool::new(true))).await;

    // A list URL and the list-view URL from the address bar both resolve.
    for url in [
        format!("https://app.clickup.com/8413555/v/li/{}", fx.ids.synott),
        "https://app.clickup.com/8413555/v/l/80rbk-1".to_string(),
    ] {
        let response = fx.resolve(&url).await;
        assert_eq!(response.status(), StatusCode::OK, "{url}");
        let body = body_json(response).await;
        assert_eq!(body["list_name"], "Affordable Storage Synott");
        assert_eq!(body["folder_name"], "Affordable Storage - Beau Ryan");
    }

    // Not ClickUp / not a list / a list in another space / nonexistent.
    assert_eq!(
        fx.resolve("https://example.com/x").await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        fx.resolve("https://app.clickup.com/8413555/v/o/f/901410626857")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        fx.resolve("https://app.clickup.com/8413555/v/l/folder-1")
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let outside = fx
        .resolve(&format!(
            "https://app.clickup.com/8413555/v/li/{}",
            fx.ids.other_space
        ))
        .await;
    assert_eq!(outside.status(), StatusCode::BAD_REQUEST);
    assert!(body_json(outside).await["message"]
        .as_str()
        .unwrap()
        .contains("QMS Onboarding"));
    assert_eq!(
        fx.resolve("https://app.clickup.com/8413555/v/li/999999")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );

    // A mistyped list URL must NOT mark the user's good token invalid.
    let status = clickup_connection::get_connection(State(fx.state.clone()), fx.manager()).await;
    assert_eq!(body_json(status).await["status"], "connected");

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_links_db_saving_stores_what_clickup_reports_and_unlinking_clears_it() {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new(Arc::new(AtomicBool::new(true))).await;

    let saved = fx
        .save(
            fx.manager(),
            &[
                (fx.synott, &fx.ids.synott),
                (fx.copperfield, &fx.ids.copperfield),
            ],
        )
        .await;
    assert_eq!(saved.status(), StatusCode::OK);
    let saved = body_json(saved).await;
    assert_eq!(saved["linked"], 2);
    assert!(saved["shared_lists"].as_array().unwrap().is_empty());

    let (id, name, url) = fx.link_columns(fx.synott).await;
    assert_eq!(id.as_deref(), Some(fx.ids.synott.as_str()));
    // Name and URL come from ClickUp, not from the request.
    assert_eq!(name.as_deref(), Some("Affordable Storage Synott"));
    assert_eq!(
        url.as_deref(),
        Some(format!("https://app.clickup.com/8413555/v/li/{}", fx.ids.synott).as_str())
    );

    // The suggestions endpoint reports each facility's current link
    // (facilities come back ordered by name: Copperfield, then Synott).
    let suggestions = clickup_lookup::clickup_suggestions(
        State(fx.state.clone()),
        fx.manager(),
        Path(fx.company_id),
    )
    .await;
    let suggestions = body_json(suggestions).await;
    assert_eq!(
        suggestions["facilities"][1]["current"]["list_id"],
        fx.ids.synott
    );

    // Unlink one: idempotent.
    for expected in [1, 0] {
        let response = fx.unlink_one(fx.manager(), fx.synott).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["unlinked"], expected);
    }
    assert_eq!(fx.link_columns(fx.synott).await, (None, None, None));
    assert_eq!(
        fx.link_columns(fx.copperfield).await.0.as_deref(),
        Some(fx.ids.copperfield.as_str())
    );

    // Unlink all clears the rest.
    let all = clients_clickup_links::unlink_company_clickup(
        State(fx.state.clone()),
        fx.manager(),
        HeaderMap::new(),
        Path(fx.company_id),
    )
    .await;
    assert_eq!(body_json(all).await["unlinked"], 1);
    assert_eq!(fx.link_columns(fx.copperfield).await, (None, None, None));

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_links_db_a_bad_list_saves_nothing_at_all() {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new(Arc::new(AtomicBool::new(true))).await;

    // One good link and one list in the wrong space: the whole request is
    // refused and the good one is NOT saved either.
    let response = fx
        .save(
            fx.manager(),
            &[
                (fx.synott, &fx.ids.synott),
                (fx.copperfield, &fx.ids.other_space),
            ],
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(fx.link_columns(fx.synott).await, (None, None, None));
    assert_eq!(fx.link_columns(fx.copperfield).await, (None, None, None));

    // A facility from a different company is refused as not found.
    let other_company: Uuid = sqlx::query_scalar(
        "INSERT INTO clients.companies (legal_name, source) VALUES ('Someone Else', 'manual') RETURNING id",
    )
    .fetch_one(&fx.superuser)
    .await
    .unwrap();
    let foreign: Uuid = sqlx::query_scalar(
        "INSERT INTO clients.facilities (company_id, name, source) VALUES ($1, 'Foreign', 'manual') RETURNING id",
    )
    .bind(other_company)
    .fetch_one(&fx.superuser)
    .await
    .unwrap();
    let response = fx.save(fx.manager(), &[(foreign, &fx.ids.synott)]).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_links_db_a_user_without_a_client_ops_role_cannot_write_links() {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new(Arc::new(AtomicBool::new(true))).await;

    // Holds integrations.clickup but no client-ops role: RLS narrows the
    // UPDATE to nothing, which must surface as a 403, not a silent no-op
    // and not a misleading "no such facility".
    let response = fx
        .save(fx.no_client_ops_role(), &[(fx.synott, &fx.ids.synott)])
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(fx.link_columns(fx.synott).await, (None, None, None));

    // Likewise for unlinking a link that exists.
    assert_eq!(
        fx.save(fx.manager(), &[(fx.synott, &fx.ids.synott)])
            .await
            .status(),
        StatusCode::OK
    );
    let response = fx.unlink_one(fx.no_client_ops_role(), fx.synott).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        fx.link_columns(fx.synott).await.0.as_deref(),
        Some(fx.ids.synott.as_str())
    );

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_links_db_linking_a_list_another_facility_already_uses_is_allowed_but_reported() {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new(Arc::new(AtomicBool::new(true))).await;

    assert_eq!(
        fx.save(fx.manager(), &[(fx.synott, &fx.ids.synott)])
            .await
            .status(),
        StatusCode::OK
    );

    let response = fx
        .save(fx.manager(), &[(fx.copperfield, &fx.ids.synott)])
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["linked"], 1);
    assert_eq!(body["shared_lists"][0]["list_id"], fx.ids.synott);
    assert_eq!(
        body["shared_lists"][0]["also_linked_to"],
        json!(["Affordable Storage Synott"])
    );

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_links_db_unlinking_works_even_when_the_users_token_has_gone_bad() {
    let _ = dotenvy::from_filename(".env.local");
    let accepting = Arc::new(AtomicBool::new(true));
    let fx = Fixture::new(accepting.clone()).await;

    assert_eq!(
        fx.save(fx.manager(), &[(fx.synott, &fx.ids.synott)])
            .await
            .status(),
        StatusCode::OK
    );

    // The user's token is revoked in ClickUp. A lookup that calls ClickUp
    // now fails with the actionable "token invalid" conflict and flips the
    // stored status. (A list already in the cached hierarchy is confirmed
    // without calling ClickUp, so ask about one that is not.)
    accepting.store(false, Ordering::SeqCst);
    let lookup = fx
        .resolve(&format!(
            "https://app.clickup.com/8413555/v/li/{}",
            fx.ids.other_space
        ))
        .await;
    assert_eq!(lookup.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(lookup).await["error"], "clickup_token_invalid");
    let status = clickup_connection::get_connection(State(fx.state.clone()), fx.manager()).await;
    assert_eq!(body_json(status).await["status"], "invalid");

    // ...after which every ClickUp-backed call short-circuits with the same
    // conflict instead of hitting ClickUp again...
    let again = clickup_lookup::list_clickup_lists(State(fx.state.clone()), fx.manager()).await;
    assert_eq!(again.status(), StatusCode::CONFLICT);

    // ...but removing a stale link needs no ClickUp call, so it still works.
    let response = fx.unlink_one(fx.manager(), fx.synott).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fx.link_columns(fx.synott).await, (None, None, None));

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn clickup_links_db_a_user_who_never_connected_clickup_is_told_to_connect_it() {
    let _ = dotenvy::from_filename(".env.local");
    let superuser = superuser_pool();
    let user_id = create_user(&superuser, "unconnected").await;
    let state = AppState {
        db: crate::db::connect_test(),
        ..empty_state()
    };

    let response = clickup_lookup::list_clickup_lists(
        State(state),
        Fixture::clickup_caller_for(user_id, &["onboarding_manager"]),
    )
    .await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(body_json(response).await["error"], "clickup_not_connected");
}
