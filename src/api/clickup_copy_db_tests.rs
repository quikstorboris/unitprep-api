//! Real-database tests for ClickUp Copy (`clickup_copy`): pairing two
//! facilities' lists, the per-row comment lookup, and copying comments
//! with the once-per-task main-list pointer -- against a mock ClickUp (two
//! lists, custom fields, comment state) and the local `test-db` (as
//! `app_service`, so RLS applies). Every test is `#[ignore]`d -- see
//! `clickup_db_tests`' module doc for how to run them.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Json, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::clickup_db_tests::{body_json, caller, create_user, superuser_pool};
use crate::api::test_support::{empty_state, FakeEnvSource};
use crate::api::{clickup_connection, clickup_copy, AppState};
use crate::auth::AuthenticatedUser;

const ENCRYPTION_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";

type Comments = Arc<Mutex<HashMap<String, Vec<Value>>>>;
type Writes = Arc<Mutex<Vec<(String, Value)>>>;

#[derive(Clone)]
struct Mock {
    source_list: String,
    target_list: String,
    comments: Comments,
    writes: Writes,
    clock: Arc<AtomicI64>,
}

/// A task as ClickUp sends it, with the Onboarding Phase and Corp/Fac
/// dropdowns carrying their option *index* as the value.
fn task(id: &str, name: &str, parent: Option<&str>, list: &str, phase: i64, scope: i64) -> Value {
    json!({
        "id": id, "name": name, "parent": parent,
        "status": { "status": "to do", "type": "open" },
        "assignees": [],
        "url": format!("https://app.clickup.com/t/{id}"),
        "list": { "id": list },
        "custom_fields": [
            { "id": "f-phase", "name": "Onboarding Phase", "type": "drop_down",
              "type_config": { "options": [
                  { "id": "o-setup", "name": "Set Up", "orderindex": 0 },
                  { "id": "o-mig", "name": "Migration", "orderindex": 1 },
                  { "id": "o-train", "name": "Training", "orderindex": 2 }
              ] }, "value": phase },
            { "id": "f-scope", "name": "Corp/Fac", "type": "drop_down",
              "type_config": { "options": [
                  { "id": "o-corp", "name": "Corporate", "orderindex": 0 },
                  { "id": "o-fac", "name": "Facility", "orderindex": 1 }
              ] }, "value": scope }
        ]
    })
}

/// The same template in both lists: two Set Up tasks, one Migration task
/// and one Training task (never copied). Task ids carry a prefix (`S` in
/// the source list, `T` in the target list) because ClickUp's ids are
/// unique per task, and so the mock's comment store keeps each list's
/// tasks apart.
fn list_tasks(list: &str, prefix: &str) -> Value {
    let id = |name: &str| format!("{prefix}{name}");
    json!({ "last_page": true, "tasks": [
        task(&id("fees"), "CONFIGURE Fees", None, list, 0, 0),
        task(&id("delinq"), "CONFIGURE Delinquency", None, list, 0, 0),
        task(&id("import"), "IMPORT Tenants", None, list, 1, 1),
        task(&id("train"), "Train Staff", None, list, 2, 1)
    ] })
}

fn comment_json(id: String, text: &str, date_ms: i64) -> Value {
    json!({ "id": id, "comment_text": text, "date": date_ms.to_string(), "user": { "username": "Ann" } })
}

async fn spawn_clickup(mock: Mock) -> String {
    let (m_tasks, m_get, m_post) = (mock.clone(), mock.clone(), mock.clone());

    let app = Router::new()
        .route(
            "/user",
            get(|| async { Json(json!({ "user": { "id": 42, "username": "Test Person" } })) }),
        )
        .route(
            "/team",
            get(|| async { Json(json!({ "teams": [ { "id": "8413555", "name": "QuikStor" } ] })) }),
        )
        .route(
            "/list/{id}/task",
            get(move |Path(id): Path<String>| {
                let prefix = if id == m_tasks.source_list {
                    Some("S")
                } else if id == m_tasks.target_list {
                    Some("T")
                } else {
                    None
                };
                async move {
                    match prefix {
                        Some(prefix) => Json(list_tasks(&id, prefix)),
                        None => Json(json!({ "last_page": true, "tasks": [] })),
                    }
                }
            }),
        )
        .route(
            "/task/{id}/comment",
            get(move |Path(id): Path<String>| {
                let comments = m_get.comments.clone();
                async move {
                    let all = comments
                        .lock()
                        .unwrap()
                        .get(&id)
                        .cloned()
                        .unwrap_or_default();
                    Json(json!({ "comments": all }))
                }
            })
            .post(move |Path(id): Path<String>, Json(body): Json<Value>| {
                let mock = m_post.clone();
                async move {
                    // A task the user may not comment on.
                    if id == "Tdelinq" {
                        return (StatusCode::FORBIDDEN, Json(json!({ "err": "no" })));
                    }
                    let text: String = body["comment"]
                        .as_array()
                        .map(|blocks| {
                            blocks
                                .iter()
                                .filter_map(|b| b["text"].as_str())
                                .collect::<String>()
                        })
                        .unwrap_or_default();
                    mock.writes.lock().unwrap().push((id.clone(), body));
                    let when = mock.clock.fetch_add(1, Ordering::SeqCst);
                    mock.comments
                        .lock()
                        .unwrap()
                        .entry(id.clone())
                        .or_default()
                        .push(comment_json(format!("w{when}"), &text, when));
                    (StatusCode::OK, Json(json!({ "id": 1 })))
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

struct Fixture {
    state: AppState,
    superuser: PgPool,
    mock: Mock,
    user_id: Uuid,
    company_id: Uuid,
    /// The company's parent: linked to the source list.
    parent: Uuid,
    /// The facility being copied to: linked to the target list.
    target: Uuid,
}

impl Fixture {
    async fn new() -> Self {
        std::env::set_var("INTEGRATION_SECRETS_ENCRYPTION_KEY", ENCRYPTION_KEY);
        let unique = || format!("{}", Uuid::new_v4().as_u128() % 900_000_000 + 100_000_000);
        let mock = Mock {
            source_list: unique(),
            target_list: unique(),
            comments: Comments::default(),
            writes: Writes::default(),
            clock: Arc::new(AtomicI64::new(1_000)),
        };
        let base_url = spawn_clickup(mock.clone()).await;
        let superuser = superuser_pool();
        let user_id = create_user(&superuser, "copy").await;

        let state = AppState {
            db: crate::db::connect_test(),
            env_source: Arc::new(FakeEnvSource::with(&[("CLICKUP_API_BASE_URL", &base_url)])),
            ..empty_state()
        };
        let saved = clickup_connection::save_token(
            State(state.clone()),
            Self::user_for(user_id),
            ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))),
            HeaderMap::new(),
            Json(clickup_connection::SaveTokenRequest {
                token: "pk_good".to_string(),
            }),
        )
        .await;
        assert_eq!(saved.status(), StatusCode::OK);

        let company_id: Uuid = sqlx::query_scalar(
            "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
        )
        .bind(format!("Copy Co {}", Uuid::new_v4()))
        .fetch_one(&superuser)
        .await
        .unwrap();

        let fixture = Self {
            state,
            parent: Self::facility(&superuser, company_id, "Main St").await,
            target: Self::facility(&superuser, company_id, "Second St").await,
            superuser,
            mock,
            user_id,
            company_id,
        };
        fixture
            .link(fixture.parent, &fixture.mock.source_list)
            .await;
        fixture
            .link(fixture.target, &fixture.mock.target_list)
            .await;
        fixture.set_parent(Some(fixture.parent)).await;
        fixture
    }

    async fn facility(superuser: &PgPool, company_id: Uuid, name: &str) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO clients.facilities (company_id, name, source) VALUES ($1, $2, 'manual') RETURNING id",
        )
        .bind(company_id)
        .bind(name)
        .fetch_one(superuser)
        .await
        .unwrap()
    }

    async fn link(&self, facility: Uuid, list: &str) {
        sqlx::query(
            "UPDATE clients.facilities
                SET clickup_list_id = $2, clickup_list_name = $3,
                    clickup_list_url = 'https://app.clickup.com/8413555/v/li/1', clickup_linked_at = now()
              WHERE id = $1",
        )
        .bind(facility)
        .bind(list)
        .bind(format!("List {list}"))
        .execute(&self.superuser)
        .await
        .unwrap();
    }

    async fn set_parent(&self, facility: Option<Uuid>) {
        sqlx::query("UPDATE clients.companies SET clickup_parent_facility_id = $2 WHERE id = $1")
            .bind(self.company_id)
            .bind(facility)
            .execute(&self.superuser)
            .await
            .unwrap();
    }

    fn user_for(user_id: Uuid) -> AuthenticatedUser {
        caller(user_id, &["onboarding_manager"], &["integrations.clickup"])
    }

    fn put_comment(&self, task_id: &str, text: &str, date_ms: i64) {
        self.mock
            .comments
            .lock()
            .unwrap()
            .entry(task_id.to_string())
            .or_default()
            .push(comment_json(format!("seed{date_ms}"), text, date_ms));
    }

    async fn pairs(&self, source: Option<Uuid>, scope: Option<&str>) -> axum::response::Response {
        clickup_copy::copy_pairs(
            State(self.state.clone()),
            Self::user_for(self.user_id),
            Path((self.company_id, self.target)),
            Query(clickup_copy::PairsQuery {
                source_facility_id: source,
                scope: scope.map(str::to_string),
            }),
        )
        .await
    }

    async fn comments(&self, source_task: &str, target_task: &str) -> axum::response::Response {
        clickup_copy::copy_comments(
            State(self.state.clone()),
            Self::user_for(self.user_id),
            Path((self.company_id, self.target)),
            Query(clickup_copy::CommentsQuery {
                source_task_id: source_task.to_string(),
                target_task_id: target_task.to_string(),
                source_facility_id: None,
            }),
        )
        .await
    }

    async fn copy(&self, items: &[(&str, &str)]) -> axum::response::Response {
        clickup_copy::copy_comments_to_tasks(
            State(self.state.clone()),
            Self::user_for(self.user_id),
            HeaderMap::new(),
            Path((self.company_id, self.target)),
            Json(clickup_copy::CopyRequest {
                source_facility_id: None,
                items: items
                    .iter()
                    .map(|(task, comment)| clickup_copy::CopyItem {
                        target_task_id: task.to_string(),
                        comment: comment.to_string(),
                    })
                    .collect(),
            }),
        )
        .await
    }

    /// Texts posted so far, as `task: text`.
    fn written(&self) -> Vec<String> {
        self.mock
            .writes
            .lock()
            .unwrap()
            .iter()
            .map(|(task, body)| {
                let text: String = body["comment"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|b| b["text"].as_str())
                    .collect();
                format!("{task}: {text}")
            })
            .collect()
    }
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_pairs_set_up_and_migration_tasks_and_skips_other_phases() {
    let f = Fixture::new().await;

    let response = f.pairs(None, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    // The source defaults to the parent; the pointer names the parent.
    assert_eq!(body["source"]["facility_name"], "Main St");
    assert_eq!(body["target"]["facility_name"], "Second St");
    assert_eq!(body["parent"]["facility_name"], "Main St");

    let rows = body["rows"].as_array().unwrap();
    let ids: Vec<&str> = rows
        .iter()
        .map(|row| row["source"]["task_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 3, "Training must not be offered: {ids:?}");
    assert!(!ids.contains(&"Strain"));

    let fees = rows
        .iter()
        .find(|row| row["source"]["task_id"] == "Sfees")
        .unwrap();
    assert_eq!(fees["phase"], "Set Up");
    assert_eq!(fees["target"]["task_id"], "Tfees");
    assert_eq!(fees["source"]["scope"], "corporate");

    let import = rows
        .iter()
        .find(|row| row["source"]["task_id"] == "Simport")
        .unwrap();
    assert_eq!(import["phase"], "Migration");
    assert_eq!(import["target"]["task_id"], "Timport");

    assert_eq!(body["target_tasks"].as_array().unwrap().len(), 3);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_the_scope_filter_narrows_the_rows() {
    let f = Fixture::new().await;

    let corporate = body_json(f.pairs(None, Some("corporate")).await).await;
    assert_eq!(corporate["rows"].as_array().unwrap().len(), 2);

    let facility = body_json(f.pairs(None, Some("facility")).await).await;
    assert_eq!(facility["rows"].as_array().unwrap().len(), 1);

    assert_eq!(
        f.pairs(None, Some("nonsense")).await.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_refuses_without_a_source_or_when_the_source_is_the_target_or_unlinked() {
    let f = Fixture::new().await;

    // The target is itself: nothing to copy from.
    assert_eq!(
        f.pairs(Some(f.target), None).await.status(),
        StatusCode::BAD_REQUEST
    );

    // No parent and no explicit source.
    f.set_parent(None).await;
    assert_eq!(f.pairs(None, None).await.status(), StatusCode::CONFLICT);

    // An explicit source with no list linked.
    let unlinked = Fixture::facility(&f.superuser, f.company_id, "Third St").await;
    assert_eq!(
        f.pairs(Some(unlinked), None).await.status(),
        StatusCode::CONFLICT
    );

    // A facility of some other company.
    let other_company: Uuid = sqlx::query_scalar(
        "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
    )
    .bind(format!("Other {}", Uuid::new_v4()))
    .fetch_one(&f.superuser)
    .await
    .unwrap();
    let foreign = Fixture::facility(&f.superuser, other_company, "Foreign").await;
    assert_eq!(
        f.pairs(Some(foreign), None).await.status(),
        StatusCode::BAD_REQUEST
    );

    // An explicit linked sibling works without any parent.
    let sibling = Fixture::facility(&f.superuser, f.company_id, "Fourth St").await;
    f.link(sibling, &f.mock.source_list).await;
    assert_eq!(f.pairs(Some(sibling), None).await.status(), StatusCode::OK);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_a_row_is_prefilled_with_the_sources_latest_comment() {
    let f = Fixture::new().await;
    f.put_comment("Sfees", "Older note", 100);
    f.put_comment("Sfees", "Latest note", 200);

    let body = body_json(f.comments("Sfees", "Tfees").await).await;

    assert_eq!(body["source_comment"]["text"], "Latest note");
    assert_eq!(body["already_copied"], false);
    assert_eq!(body["pointer_present"], false);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_flags_a_target_that_already_has_the_comment_or_the_pointer() {
    let f = Fixture::new().await;
    f.put_comment("Sfees", "Fees are configured.", 200);
    // The target already reads the same, differing only in case and
    // punctuation.
    f.put_comment("Tfees", "fees are configured", 300);
    f.put_comment("Timport", "Main task list for this client is Somewhere", 50);
    f.put_comment("Simport", "Tenants imported.", 60);

    let fees = body_json(f.comments("Sfees", "Tfees").await).await;
    assert_eq!(fees["already_copied"], true);
    assert_eq!(fees["pointer_present"], false);

    let import = body_json(f.comments("Simport", "Timport").await).await;
    assert_eq!(import["already_copied"], false);
    assert_eq!(import["pointer_present"], true);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_posts_the_comment_and_one_pointer_per_task() {
    let f = Fixture::new().await;

    let first = body_json(f.copy(&[("Tfees", "Fees are configured.")]).await).await;
    assert_eq!(first["copied"], 1);
    assert_eq!(first["results"][0]["comment"]["ok"], true);
    assert_eq!(first["results"][0]["pointer"]["state"], "posted");

    // A second copy to the same task adds its comment but not a second pointer.
    let second = body_json(f.copy(&[("Tfees", "Fees changed.")]).await).await;
    assert_eq!(second["results"][0]["pointer"]["state"], "already_present");

    let written = f.written();
    assert_eq!(written.len(), 3, "{written:?}");
    assert_eq!(written[0], "Tfees: Fees are configured.");
    assert!(written[1].starts_with("Tfees: Main task list for this client is List "));
    assert_eq!(written[2], "Tfees: Fees changed.");

    // The pointer's list name is a link to the parent's list.
    let pointer = f.mock.writes.lock().unwrap()[1].1.clone();
    assert!(pointer["comment"][1]["attributes"]["link"].is_string());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_the_parents_own_tasks_get_no_pointer() {
    let f = Fixture::new().await;
    // Copy *into* the parent from a sibling: the parent is the target.
    let sibling = Fixture::facility(&f.superuser, f.company_id, "Sibling St").await;
    f.link(sibling, &f.mock.target_list).await;

    let response = clickup_copy::copy_comments_to_tasks(
        State(f.state.clone()),
        Fixture::user_for(f.user_id),
        HeaderMap::new(),
        Path((f.company_id, f.parent)),
        Json(clickup_copy::CopyRequest {
            source_facility_id: Some(sibling),
            items: vec![clickup_copy::CopyItem {
                // A task in the parent's (source) list.
                target_task_id: "Sfees".to_string(),
                comment: "Done at the sibling.".to_string(),
            }],
        }),
    )
    .await;

    let body = body_json(response).await;
    assert_eq!(body["results"][0]["comment"]["ok"], true);
    assert_eq!(body["results"][0]["pointer"]["state"], "not_applicable");
    assert_eq!(f.written().len(), 1);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_without_a_parent_no_pointer_is_posted() {
    let f = Fixture::new().await;
    // An explicit source still works with no parent designated.
    f.set_parent(None).await;

    let response = clickup_copy::copy_comments_to_tasks(
        State(f.state.clone()),
        Fixture::user_for(f.user_id),
        HeaderMap::new(),
        Path((f.company_id, f.target)),
        Json(clickup_copy::CopyRequest {
            source_facility_id: Some(f.parent),
            items: vec![clickup_copy::CopyItem {
                target_task_id: "Tfees".to_string(),
                comment: "Hello.".to_string(),
            }],
        }),
    )
    .await;

    let body = body_json(response).await;
    assert_eq!(body["results"][0]["pointer"]["state"], "not_applicable");
    assert_eq!(f.written(), vec!["Tfees: Hello.".to_string()]);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_one_denied_row_does_not_stop_the_others() {
    let f = Fixture::new().await;

    let body = body_json(
        f.copy(&[("Tdelinq", "Will be denied."), ("Tfees", "Goes through.")])
            .await,
    )
    .await;

    assert_eq!(body["copied"], 1);
    assert_eq!(body["failed"], 1);
    assert_eq!(body["results"][0]["comment"]["ok"], false);
    assert!(body["results"][0]["comment"]["message"]
        .as_str()
        .unwrap()
        .contains("Access denied"));
    // A failed row gets no pointer; the other row is unaffected.
    assert_eq!(body["results"][0]["pointer"]["state"], "not_applicable");
    assert_eq!(body["results"][1]["comment"]["ok"], true);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_refuses_bad_requests_without_writing() {
    let f = Fixture::new().await;

    // A task that is not in the facility's list.
    assert_eq!(
        f.copy(&[("elsewhere", "Nope.")]).await.status(),
        StatusCode::BAD_REQUEST
    );
    // An empty comment, and an empty request.
    assert_eq!(
        f.copy(&[("Tfees", "   ")]).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(f.copy(&[]).await.status(), StatusCode::BAD_REQUEST);
    // Too many rows at once.
    let many: Vec<(&str, &str)> = vec![("Tfees", "x"); 31];
    assert_eq!(f.copy(&many).await.status(), StatusCode::BAD_REQUEST);

    assert!(f.written().is_empty());
}
