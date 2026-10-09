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
    /// Further lists (list id -> task-id prefix) for the bulk copy's extra
    /// facilities; each behaves like the source and target lists.
    extra: Arc<Mutex<HashMap<String, String>>>,
    /// Whether the lists offer a complete status (they do unless a test says not).
    has_complete_status: Arc<std::sync::atomic::AtomicBool>,
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
                  { "id": "o-train", "name": "Training", "orderindex": 2 },
                  { "id": "o-sched", "name": "Scheduling", "orderindex": 3 },
                  { "id": "o-stop", "name": "Show Stoppers", "orderindex": 4 }
              ] }, "value": phase },
            { "id": "f-scope", "name": "Corp/Fac", "type": "drop_down",
              "type_config": { "options": [
                  { "id": "o-corp", "name": "Corporate", "orderindex": 0 },
                  { "id": "o-fac", "name": "Facility", "orderindex": 1 }
              ] }, "value": scope }
        ]
    })
}

/// The same template in both lists: two Set Up tasks, one each in
/// Migration, Training, Scheduling and Show Stoppers, and one task with no
/// phase at all (never offered). Task ids carry a prefix (`S` in the
/// source list, `T` in the target list) because ClickUp's ids are unique
/// per task, and so the mock's comment store keeps each list's tasks
/// apart.
fn list_tasks(list: &str, prefix: &str) -> Value {
    let id = |name: &str| format!("{prefix}{name}");
    let mut loose = task(&id("loose"), "Loose Task", None, list, 0, 1);
    loose["custom_fields"][0]["value"] = Value::Null;

    json!({ "last_page": true, "tasks": [
        task(&id("fees"), "CONFIGURE Fees", None, list, 0, 0),
        task(&id("delinq"), "CONFIGURE Delinquency", None, list, 0, 0),
        task(&id("import"), "IMPORT Tenants", None, list, 1, 1),
        task(&id("train"), "Train Staff", None, list, 2, 1),
        task(&id("sched"), "BOOK Go-Live Call", None, list, 3, 1),
        task(&id("stop"), "RESOLVE Open Blockers", None, list, 4, 0),
        loose
    ] })
}

fn comment_json(id: String, text: &str, date_ms: i64) -> Value {
    json!({ "id": id, "comment_text": text, "date": date_ms.to_string(), "user": { "username": "Ann" } })
}

async fn spawn_clickup(mock: Mock) -> String {
    let (m_tasks, m_get, m_post) = (mock.clone(), mock.clone(), mock.clone());
    let (m_statuses, m_put) = (mock.clone(), mock.clone());

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
                let prefix: Option<String> = if id == m_tasks.source_list {
                    Some("S".to_string())
                } else if id == m_tasks.target_list {
                    Some("T".to_string())
                } else {
                    m_tasks.extra.lock().unwrap().get(&id).cloned()
                };
                async move {
                    match prefix {
                        Some(prefix) => Json(list_tasks(&id, &prefix)),
                        None => Json(json!({ "last_page": true, "tasks": [] })),
                    }
                }
            }),
        )
        .route(
            "/list/{id}",
            get(move |Path(_id): Path<String>| {
                let has_complete = m_statuses
                    .has_complete_status
                    .load(std::sync::atomic::Ordering::SeqCst);
                async move {
                    let mut statuses = vec![json!({ "status": "to do", "type": "open" })];
                    if has_complete {
                        statuses.push(json!({ "status": "complete", "type": "closed" }));
                    }
                    Json(json!({ "id": "L", "name": "list", "statuses": statuses }))
                }
            }),
        )
        .route(
            "/task/{id}",
            axum::routing::put(move |Path(id): Path<String>, Json(body): Json<Value>| {
                let writes = m_put.writes.clone();
                async move {
                    writes.lock().unwrap().push((id.clone(), body));
                    Json(json!({ "id": id }))
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
            extra: Arc::default(),
            has_complete_status: Arc::new(std::sync::atomic::AtomicBool::new(true)),
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

    /// A further facility of the company with its own list in the mock;
    /// returns it and the prefix its task ids carry (`U1`, `U2`, ...).
    async fn add_facility(&self, name: &str) -> (Uuid, String) {
        let facility = Self::facility(&self.superuser, self.company_id, name).await;
        let list = format!("{}", Uuid::new_v4().as_u128() % 900_000_000 + 100_000_000);
        let prefix = {
            let mut extra = self.mock.extra.lock().unwrap();
            let prefix = format!("U{}", extra.len() + 1);
            extra.insert(list.clone(), prefix.clone());
            prefix
        };
        self.link(facility, &list).await;
        (facility, prefix)
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
                complete_tasks: false,
                items: items
                    .iter()
                    .map(|(task, comment)| clickup_copy::CopyItem {
                        target_task_id: task.to_string(),
                        comment: comment.to_string(),
                        source_task_id: None,
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
async fn copy_db_pairs_tasks_in_every_phase_in_the_templates_order() {
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
    // Every phase is offered; a task with no phase is not.
    assert_eq!(ids.len(), 6, "{ids:?}");
    assert!(ids.contains(&"Sstop") && ids.contains(&"Ssched") && ids.contains(&"Strain"));
    assert!(!ids.contains(&"Sloose"));

    // Each row carries its phase and the phase's place in the template's
    // own order, which the page sorts the groups by.
    let order_of = |id: &str| {
        rows.iter()
            .find(|row| row["source"]["task_id"] == id)
            .unwrap()["phase_order"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(
        ["Sfees", "Simport", "Strain", "Ssched", "Sstop"].map(order_of),
        [0, 1, 2, 3, 4]
    );
    let stop = rows
        .iter()
        .find(|row| row["source"]["task_id"] == "Sstop")
        .unwrap();
    assert_eq!(stop["phase"], "Show Stoppers");
    assert_eq!(stop["target"]["task_id"], "Tstop");

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

    assert_eq!(body["target_tasks"].as_array().unwrap().len(), 6);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_the_scope_filter_narrows_the_rows() {
    let f = Fixture::new().await;

    let corporate = body_json(f.pairs(None, Some("corporate")).await).await;
    // Corporate: fees, delinquency, open blockers.
    assert_eq!(corporate["rows"].as_array().unwrap().len(), 3);

    let facility = body_json(f.pairs(None, Some("facility")).await).await;
    // Facility: import, training, go-live call (the phase-less task is
    // never offered, whatever its scope).
    assert_eq!(facility["rows"].as_array().unwrap().len(), 3);

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
            complete_tasks: false,
            source_facility_id: Some(sibling),
            items: vec![clickup_copy::CopyItem {
                // A task in the parent's (source) list.
                target_task_id: "Sfees".to_string(),
                comment: "Done at the sibling.".to_string(),
                source_task_id: None,
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
            complete_tasks: false,
            source_facility_id: Some(f.parent),
            items: vec![clickup_copy::CopyItem {
                target_task_id: "Tfees".to_string(),
                comment: "Hello.".to_string(),
                source_task_id: None,
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

// ---------------------------------------------------------------- bulk

impl Fixture {
    async fn bulk_tasks(&self, scope: Option<&str>) -> axum::response::Response {
        clickup_copy::bulk_tasks(
            State(self.state.clone()),
            Self::user_for(self.user_id),
            Path(self.company_id),
            Query(clickup_copy::BulkTasksQuery {
                source_facility_id: None,
                scope: scope.map(str::to_string),
            }),
        )
        .await
    }

    async fn bulk_pairs(&self, source_task: &str) -> axum::response::Response {
        clickup_copy::bulk_pairs(
            State(self.state.clone()),
            Self::user_for(self.user_id),
            Path(self.company_id),
            Query(clickup_copy::BulkPairsQuery {
                source_facility_id: None,
                source_task_id: source_task.to_string(),
                scope: None,
            }),
        )
        .await
    }

    async fn bulk_comment(&self, source_task: &str) -> axum::response::Response {
        clickup_copy::bulk_comment(
            State(self.state.clone()),
            Self::user_for(self.user_id),
            Path(self.company_id),
            Query(clickup_copy::BulkCommentQuery {
                source_facility_id: None,
                source_task_id: source_task.to_string(),
            }),
        )
        .await
    }

    async fn bulk_copy(
        &self,
        comment: &str,
        destinations: &[(Uuid, &str)],
    ) -> axum::response::Response {
        self.bulk_copy_from(None, comment, destinations).await
    }

    /// A bulk copy that names the source task, so the comments end with
    /// the "Main tracker task" link.
    async fn bulk_copy_from(
        &self,
        source_task_id: Option<&str>,
        comment: &str,
        destinations: &[(Uuid, &str)],
    ) -> axum::response::Response {
        self.bulk_copy_full(source_task_id, false, comment, destinations)
            .await
    }

    /// A bulk copy that also asks for the destination tasks to be completed.
    async fn bulk_copy_completing(
        &self,
        comment: &str,
        destinations: &[(Uuid, &str)],
    ) -> axum::response::Response {
        self.bulk_copy_full(None, true, comment, destinations).await
    }

    async fn bulk_copy_full(
        &self,
        source_task_id: Option<&str>,
        complete_tasks: bool,
        comment: &str,
        destinations: &[(Uuid, &str)],
    ) -> axum::response::Response {
        clickup_copy::bulk_copy(
            State(self.state.clone()),
            Self::user_for(self.user_id),
            HeaderMap::new(),
            Path(self.company_id),
            Json(clickup_copy::BulkCopyRequest {
                source_facility_id: None,
                source_task_name: "CONFIGURE Fees".to_string(),
                source_task_id: source_task_id.map(str::to_string),
                complete_tasks,
                comment: comment.to_string(),
                destinations: destinations
                    .iter()
                    .map(|(facility_id, task)| clickup_copy::BulkDestination {
                        facility_id: *facility_id,
                        target_task_id: task.to_string(),
                    })
                    .collect(),
            }),
        )
        .await
    }
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_lists_the_source_tasks_and_the_possible_destinations() {
    let f = Fixture::new().await;
    let (extra, _) = f.add_facility("Third St").await;
    let unlinked = Fixture::facility(&f.superuser, f.company_id, "Fourth St").await;

    let body = body_json(f.bulk_tasks(None).await).await;

    assert_eq!(body["source"]["facility_name"], "Main St");
    assert_eq!(body["parent"]["facility_name"], "Main St");
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(
        tasks.len(),
        6,
        "every phase is offered, a phase-less task is not"
    );
    assert_eq!(
        tasks.iter().find(|t| t["task_id"] == "Simport").unwrap()["phase"],
        "Migration"
    );
    let stop = tasks.iter().find(|t| t["task_id"] == "Sstop").unwrap();
    assert_eq!(stop["phase"], "Show Stoppers");
    assert_eq!(stop["phase_order"], 4);

    let destinations: Vec<&str> = body["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["facility_name"].as_str().unwrap())
        .collect();
    assert_eq!(destinations, vec!["Second St", "Third St"]);
    assert!(body["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["facility_id"] == extra.to_string()));
    // A facility with no list is named, but not offered.
    assert_eq!(body["unlinked"][0]["facility_name"], "Fourth St");
    assert_eq!(body["unlinked"][0]["facility_id"], unlinked.to_string());

    let corporate = body_json(f.bulk_tasks(Some("corporate")).await).await;
    assert_eq!(corporate["tasks"].as_array().unwrap().len(), 3);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_pairs_the_chosen_task_in_every_destination() {
    let f = Fixture::new().await;
    f.add_facility("Third St").await;

    let body = body_json(f.bulk_pairs("Sfees").await).await;

    let destinations = body["destinations"].as_array().unwrap();
    assert_eq!(destinations.len(), 2);
    let second = destinations
        .iter()
        .find(|d| d["facility_name"] == "Second St")
        .unwrap();
    assert_eq!(second["target"]["task_id"], "Tfees");
    assert_eq!(second["tasks"].as_array().unwrap().len(), 6);
    let third = destinations
        .iter()
        .find(|d| d["facility_name"] == "Third St")
        .unwrap();
    assert_eq!(third["target"]["task_id"], "U1fees");
    assert!(third["error"].is_null());

    // A task that is not in the source list.
    assert_eq!(
        f.bulk_pairs("Tfees").await.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_prefills_with_the_source_tasks_latest_comment() {
    let f = Fixture::new().await;
    f.put_comment("Sfees", "Older", 100);
    f.put_comment("Sfees", "Newest", 300);

    let body = body_json(f.bulk_comment("Sfees").await).await;

    assert_eq!(body["source_comment"]["text"], "Newest");
    let none = body_json(f.bulk_comment("Sdelinq").await).await;
    assert!(none["source_comment"].is_null());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_a_small_copy_runs_inside_the_request() {
    let f = Fixture::new().await;
    let (third, _) = f.add_facility("Third St").await;

    let response = f
        .bulk_copy(
            "Fees are configured.",
            &[(f.target, "Tfees"), (third, "U1fees")],
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["mode"], "inline");
    assert_eq!(body["copied"], 2);
    assert_eq!(body["failed"], 0);
    assert_eq!(body["results"][0]["facility_name"], "Second St");
    assert_eq!(body["results"][0]["pointer"]["state"], "posted");
    assert_eq!(body["results"][1]["facility_name"], "Third St");

    // The comment and its pointer, on each destination's own task.
    let written = f.written();
    assert_eq!(written.len(), 4, "{written:?}");
    for task in ["Tfees", "U1fees"] {
        assert!(written.contains(&format!("{task}: Fees are configured.")));
        assert!(written
            .iter()
            .any(|w| w.starts_with(&format!("{task}: Main task list for this client is List "))));
    }
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_refuses_bad_requests_without_writing() {
    let f = Fixture::new().await;
    let (third, _) = f.add_facility("Third St").await;
    let unlinked = Fixture::facility(&f.superuser, f.company_id, "Fourth St").await;

    let bad = |response: axum::response::Response| response.status() == StatusCode::BAD_REQUEST;

    // Nothing chosen; an empty comment; the same facility twice.
    assert!(bad(f.bulk_copy("Hi.", &[]).await));
    assert!(bad(f.bulk_copy("  ", &[(third, "U1fees")]).await));
    assert!(bad(f
        .bulk_copy("Hi.", &[(third, "U1fees"), (third, "U1delinq")])
        .await));
    // The source itself, a facility with no list, and a facility of nobody's.
    assert!(bad(f.bulk_copy("Hi.", &[(f.parent, "Sfees")]).await));
    assert!(bad(f.bulk_copy("Hi.", &[(unlinked, "Xfees")]).await));
    assert!(bad(f.bulk_copy("Hi.", &[(Uuid::new_v4(), "Xfees")]).await));
    // A task from another facility's list.
    assert!(bad(f.bulk_copy("Hi.", &[(third, "Tfees")]).await));

    assert!(f.written().is_empty());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_a_denied_destination_does_not_stop_the_others() {
    let f = Fixture::new().await;
    let (third, _) = f.add_facility("Third St").await;

    let body = body_json(
        f.bulk_copy("Hello.", &[(f.target, "Tdelinq"), (third, "U1fees")])
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
    assert_eq!(body["results"][1]["comment"]["ok"], true);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_a_big_copy_runs_as_a_background_job_only_its_owner_can_see() {
    let f = Fixture::new().await;
    // 17 destinations with a pointer each is 51 ClickUp calls: over the
    // inline budget of 50.
    let mut destinations: Vec<(Uuid, String)> = Vec::new();
    for n in 0..17 {
        let (facility, prefix) = f.add_facility(&format!("Facility {n:02}")).await;
        destinations.push((facility, format!("{prefix}fees")));
    }
    let refs: Vec<(Uuid, &str)> = destinations
        .iter()
        .map(|(id, t)| (*id, t.as_str()))
        .collect();

    let response = f.bulk_copy("Done everywhere.", &refs).await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = body_json(response).await;
    assert_eq!(body["mode"], "job");
    assert_eq!(body["total"], 17);
    let job_id: Uuid = body["job_id"].as_str().unwrap().parse().unwrap();

    // The job runs in the background; wait for it.
    let mut job = Value::Null;
    for _ in 0..60 {
        let response = clickup_copy::get_copy_job(
            State(f.state.clone()),
            Fixture::user_for(f.user_id),
            Path((f.company_id, job_id)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        job = body_json(response).await;
        if job["status"] != "running" {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert_eq!(job["status"], "done", "{job}");
    assert_eq!(job["total"], 17);
    assert_eq!(job["copied"], 17);
    assert_eq!(job["failed"], 0);
    assert_eq!(job["results"].as_array().unwrap().len(), 17);
    assert_eq!(job["source_task_name"], "CONFIGURE Fees");
    assert!(!job["finished_at"].is_null());

    // 17 comments and 17 pointers.
    assert_eq!(f.written().len(), 34);

    // It is in the owner's list...
    let listed = body_json(
        clickup_copy::list_copy_jobs(
            State(f.state.clone()),
            Fixture::user_for(f.user_id),
            Path(f.company_id),
        )
        .await,
    )
    .await;
    assert!(listed
        .as_array()
        .unwrap()
        .iter()
        .any(|j| j["id"] == job_id.to_string()));

    // ...and invisible to anyone else, who gets a 404, not a 403.
    let other = create_user(&f.superuser, "other").await;
    let response = clickup_copy::get_copy_job(
        State(f.state.clone()),
        Fixture::user_for(other),
        Path((f.company_id, job_id)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let listed = body_json(
        clickup_copy::list_copy_jobs(
            State(f.state.clone()),
            Fixture::user_for(other),
            Path(f.company_id),
        )
        .await,
    )
    .await;
    assert!(listed.as_array().unwrap().is_empty());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_a_job_that_has_gone_quiet_is_reported_as_interrupted() {
    let f = Fixture::new().await;
    // A job left "running" by a server that then restarted.
    let job_id: Uuid = sqlx::query_scalar(
        "INSERT INTO client_ops.clickup_copy_jobs
             (company_id, created_by, source_facility_id, source_task_name, total, updated_at)
         VALUES ($1, $2, $3, 'CONFIGURE Fees', 5, now() - interval '10 minutes') RETURNING id",
    )
    .bind(f.company_id)
    .bind(f.user_id)
    .bind(f.parent)
    .fetch_one(&f.superuser)
    .await
    .unwrap();

    let body = body_json(
        clickup_copy::get_copy_job(
            State(f.state.clone()),
            Fixture::user_for(f.user_id),
            Path((f.company_id, job_id)),
        )
        .await,
    )
    .await;

    assert_eq!(body["status"], "interrupted");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_a_dialog_copy_ends_with_a_link_to_the_source_task() {
    let f = Fixture::new().await;

    let response = clickup_copy::copy_comments_to_tasks(
        State(f.state.clone()),
        Fixture::user_for(f.user_id),
        HeaderMap::new(),
        Path((f.company_id, f.target)),
        Json(clickup_copy::CopyRequest {
            complete_tasks: false,
            source_facility_id: Some(f.parent),
            items: vec![clickup_copy::CopyItem {
                target_task_id: "Tfees".to_string(),
                comment: "Fees are set.".to_string(),
                source_task_id: Some("Sfees".to_string()),
            }],
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let writes = f.mock.writes.lock().unwrap().clone();
    let blocks = writes[0].1["comment"].as_array().unwrap();
    assert_eq!(blocks[0]["text"], "Fees are set.");
    // Three line breaks, then the label, then the source task as a link.
    assert_eq!(blocks[1]["text"], "\n\n\nMain tracker task - ");
    assert_eq!(blocks[2]["text"], "CONFIGURE Fees");
    assert_eq!(
        blocks[2]["attributes"]["link"],
        "https://app.clickup.com/t/Sfees"
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_every_copied_comment_ends_with_a_link_to_the_source_task() {
    let f = Fixture::new().await;

    let response = f
        .bulk_copy_from(Some("Sfees"), "Fees are set.", &[(f.target, "Tfees")])
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let writes = f.mock.writes.lock().unwrap().clone();
    let blocks = writes[0].1["comment"].as_array().unwrap();
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[1]["text"], "\n\n\nMain tracker task - ");
    assert_eq!(blocks[2]["text"], "CONFIGURE Fees");
    assert_eq!(
        blocks[2]["attributes"]["link"],
        "https://app.clickup.com/t/Sfees"
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_a_source_task_outside_the_source_list_is_refused_and_nothing_is_posted() {
    let f = Fixture::new().await;

    // Tfees lives in the destination's list, not the source's.
    let response = f
        .bulk_copy_from(Some("Tfees"), "Fees are set.", &[(f.target, "Tfees")])
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await["error"],
        "task_not_in_source_list"
    );
    assert!(f.mock.writes.lock().unwrap().is_empty());
}

/// Status changes the mock recorded (comment writes have a `comment` key).
fn status_writes(f: &Fixture) -> Vec<(String, Value)> {
    f.mock
        .writes
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, body)| body.get("status").is_some())
        .cloned()
        .collect()
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_only_comments_unless_completing_is_asked_for() {
    let f = Fixture::new().await;

    let response = f.bulk_copy("Fees are set.", &[(f.target, "Tfees")]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert!(status_writes(&f).is_empty());
    assert!(body["results"][0].get("completed").is_none());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_completes_each_destination_task_when_asked() {
    let f = Fixture::new().await;

    let response = f
        .bulk_copy_completing("Fees are set.", &[(f.target, "Tfees")])
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert_eq!(body["results"][0]["comment"]["ok"], true);
    assert_eq!(body["results"][0]["completed"]["ok"], true);
    let statuses = status_writes(&f);
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].0, "Tfees");
    assert_eq!(statuses[0].1, json!({ "status": "complete" }));
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_a_list_with_no_complete_status_still_gets_the_comment() {
    let f = Fixture::new().await;
    f.mock
        .has_complete_status
        .store(false, std::sync::atomic::Ordering::SeqCst);

    let response = f
        .bulk_copy_completing("Fees are set.", &[(f.target, "Tfees")])
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert_eq!(body["results"][0]["comment"]["ok"], true);
    assert_eq!(body["results"][0]["completed"]["ok"], false);
    assert_eq!(
        body["results"][0]["completed"]["message"],
        "This list has no complete status."
    );
    assert!(status_writes(&f).is_empty());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_a_task_that_cannot_be_commented_on_is_not_completed() {
    let f = Fixture::new().await;

    // Tdelinq refuses comments in the mock.
    let response = f
        .bulk_copy_completing("Delinquency set.", &[(f.target, "Tdelinq")])
        .await;
    let body = body_json(response).await;

    assert_eq!(body["results"][0]["comment"]["ok"], false);
    assert!(body["results"][0].get("completed").is_none());
    assert!(status_writes(&f).is_empty());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_the_dialog_completes_the_target_task_only_when_asked() {
    let f = Fixture::new().await;
    let request = |complete_tasks: bool| clickup_copy::CopyRequest {
        source_facility_id: Some(f.parent),
        complete_tasks,
        items: vec![clickup_copy::CopyItem {
            target_task_id: "Tfees".to_string(),
            comment: "Hello.".to_string(),
            source_task_id: None,
        }],
    };
    let post = |complete_tasks: bool| {
        clickup_copy::copy_comments_to_tasks(
            State(f.state.clone()),
            Fixture::user_for(f.user_id),
            HeaderMap::new(),
            Path((f.company_id, f.target)),
            Json(request(complete_tasks)),
        )
    };

    let without = body_json(post(false).await).await;
    assert!(without["results"][0].get("completed").is_none());
    assert!(status_writes(&f).is_empty());

    let with = body_json(post(true).await).await;
    assert_eq!(with["results"][0]["completed"]["ok"], true);
    assert_eq!(status_writes(&f).len(), 1);
}
