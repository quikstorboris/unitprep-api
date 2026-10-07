//! Real-database tests for posting a duplicate check to its ClickUp task
//! (`clickup_duplicate_check`): task candidates for the 1st vs 2nd check,
//! the refusals (facility not linked, file not saved, task in another
//! list, list without a complete status) and the three ClickUp writes,
//! against a mock ClickUp and the local `test-db` (as `app_service`, so
//! RLS applies). Every test is `#[ignore]`d -- see `clickup_db_tests`'
//! module doc for how to run them.
//!
//! The Dropbox client in the test state has fake credentials, so asking it
//! for a share link fails and the handler falls back to the plain web
//! path -- which is the branch these tests assert on.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Json, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::clickup_db_tests::{body_json, caller, create_user, superuser_pool};
use crate::api::test_support::{empty_state, FakeEnvSource};
use crate::api::{clickup_connection, clickup_duplicate_check, clickup_prefetch, AppState};
use crate::auth::AuthenticatedUser;

const ENCRYPTION_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";

type Writes = Arc<Mutex<Vec<(String, Value)>>>;

#[derive(Clone)]
struct Mock {
    list_id: String,
    has_complete_status: Arc<AtomicBool>,
    writes: Writes,
}

fn task(id: &str, name: &str, parent: Option<&str>, list_id: &str) -> Value {
    json!({
        "id": id, "name": name, "parent": parent,
        "status": { "status": "to do", "type": "open" },
        "assignees": [ { "id": 7, "username": "Someone Else" } ],
        "url": format!("https://app.clickup.com/t/{id}"),
        "list": { "id": list_id }
    })
}

async fn spawn_clickup(mock: Mock) -> String {
    let (m_tasks, m_task, m_list, m_comment, m_put) = (
        mock.clone(),
        mock.clone(),
        mock.clone(),
        mock.clone(),
        mock.clone(),
    );

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
            get(move |Path(_id): Path<String>| {
                let list = m_tasks.list_id.clone();
                async move {
                    Json(json!({ "last_page": true, "tasks": [
                        task("p1", "4. 🦕 Duplicate Tenant Corrections", None, &list),
                        task("c1", "COMPLETE Duplicate Tenant Corrections", Some("p1"), &list),
                        task("p2", "7. Second Pass", None, &list),
                        task("c2", "PERFORM 2nd Duplicate Check", Some("p2"), &list),
                        task("x1", "ADD Recurring Fees", None, &list)
                    ] }))
                }
            }),
        )
        .route(
            "/list/{id}",
            get(move |Path(_id): Path<String>| {
                let has_complete = m_list.has_complete_status.load(Ordering::SeqCst);
                async move {
                    let mut statuses = vec![json!({ "status": "to do", "type": "open" })];
                    if has_complete {
                        statuses.push(json!({ "status": "complete", "type": "closed" }));
                    }
                    Json(json!({ "id": "L", "name": "Synott", "statuses": statuses }))
                }
            }),
        )
        .route(
            "/task/{id}",
            get(move |Path(id): Path<String>| {
                let list = m_task.list_id.clone();
                async move {
                    // "alien" lives in some other list.
                    let in_list = if id == "alien" {
                        "OTHER".to_string()
                    } else {
                        list
                    };
                    Json(task(&id, "whatever", None, &in_list))
                }
            })
            .put(move |Path(id): Path<String>, Json(body): Json<Value>| {
                let writes = m_put.writes.clone();
                async move {
                    writes.lock().unwrap().push((format!("PUT {id}"), body));
                    Json(json!({ "id": id }))
                }
            }),
        )
        .route(
            "/task/{id}/comment",
            post(move |Path(id): Path<String>, Json(body): Json<Value>| {
                let writes = m_comment.writes.clone();
                async move {
                    writes.lock().unwrap().push((format!("COMMENT {id}"), body));
                    Json(json!({ "id": 1 }))
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
    facility_id: Uuid,
}

impl Fixture {
    /// A connected ClickUp user and a facility linked to the mock list.
    async fn new() -> Self {
        std::env::set_var("INTEGRATION_SECRETS_ENCRYPTION_KEY", ENCRYPTION_KEY);
        let mock = Mock {
            list_id: format!("{}", Uuid::new_v4().as_u128() % 900_000_000 + 100_000_000),
            has_complete_status: Arc::new(AtomicBool::new(true)),
            writes: Writes::default(),
        };
        let base_url = spawn_clickup(mock.clone()).await;
        let superuser = superuser_pool();
        let user_id = create_user(&superuser, "dupcheck").await;

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
            "INSERT INTO clients.companies (legal_name, source) VALUES ('Affordable Storage', 'manual') RETURNING id",
        )
        .fetch_one(&superuser)
        .await
        .unwrap();
        let facility_id: Uuid = sqlx::query_scalar(
            "INSERT INTO clients.facilities (company_id, name, city, source)
             VALUES ($1, 'Affordable Storage Synott', 'Houston', 'manual') RETURNING id",
        )
        .bind(company_id)
        .fetch_one(&superuser)
        .await
        .unwrap();

        Self {
            state,
            superuser,
            mock,
            user_id,
            company_id,
            facility_id,
        }
    }

    fn user_for(user_id: Uuid) -> AuthenticatedUser {
        caller(user_id, &["onboarding_manager"], &["integrations.clickup"])
    }

    fn user(&self) -> AuthenticatedUser {
        Self::user_for(self.user_id)
    }

    async fn link_facility(&self) {
        sqlx::query(
            "UPDATE clients.facilities
                SET clickup_list_id = $2, clickup_list_name = 'Synott',
                    clickup_list_url = 'https://app.clickup.com/8413555/v/li/1', clickup_linked_at = now()
              WHERE id = $1",
        )
        .bind(self.facility_id)
        .bind(&self.mock.list_id)
        .execute(&self.superuser)
        .await
        .unwrap();
    }

    /// Records a dedup run `minutes_ago` minutes in the past; returns its
    /// session id.
    async fn run(&self, saved_to_dropbox: bool, minutes_ago: i32) -> String {
        let session_id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO client_ops.tool_runs
                 (tool, facility_id, session_id, actor_user_id, source_file_name, report_summary,
                  output_dropbox_path, created_at)
             VALUES ('dedup', $1, $2, $3, 'Tenants.csv', '{}'::jsonb, $4,
                     now() - make_interval(mins => $5))",
        )
        .bind(self.facility_id)
        .bind(&session_id)
        .bind(self.user_id)
        .bind(saved_to_dropbox.then_some(
            "/QMS Onboarding/Affordable Storage/Synott/Duplicate Check/SYN_v1_pull_check_10-05-2026.xlsx",
        ))
        .bind(minutes_ago)
        .execute(&self.superuser)
        .await
        .unwrap();
        session_id
    }

    async fn candidates(&self, session_id: &str) -> axum::response::Response {
        clickup_duplicate_check::duplicate_check_tasks(
            State(self.state.clone()),
            self.user(),
            Path((self.company_id, self.facility_id)),
            Query(clickup_duplicate_check::CandidatesQuery {
                session_id: session_id.to_string(),
            }),
        )
        .await
    }

    async fn post(&self, session_id: &str, task_id: &str) -> axum::response::Response {
        clickup_duplicate_check::post_duplicate_check_results(
            State(self.state.clone()),
            self.user(),
            HeaderMap::new(),
            Path((self.company_id, self.facility_id)),
            Json(clickup_duplicate_check::PostResultsRequest {
                session_id: session_id.to_string(),
                task_id: task_id.to_string(),
            }),
        )
        .await
    }

    fn writes(&self) -> Vec<(String, Value)> {
        self.mock.writes.lock().unwrap().clone()
    }
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_dupcheck_db_first_and_second_checks_are_offered_their_own_tasks() {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new().await;
    fx.link_facility().await;
    let first = fx.run(true, 10).await;
    let second = fx.run(true, 5).await;

    let response = fx.candidates(&first).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["step_label"], "1st Duplicate Check");
    assert_eq!(body["sequence_number"], 1);
    assert_eq!(body["candidates"][0]["task_id"], "c1");
    assert_eq!(
        body["candidates"][0]["parent_name"],
        "4. 🦕 Duplicate Tenant Corrections"
    );
    assert_eq!(body["candidates"][0]["assignees"][0], "Someone Else");
    assert!(body["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|c| c["task_id"] != "x1"));

    let body = body_json(fx.candidates(&second).await).await;
    assert_eq!(body["step_label"], "2nd Duplicate Check");
    assert_eq!(body["candidates"][0]["task_id"], "c2");

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

/// The Onboarding Work tab lists runs by their row id, not their session
/// id; a check can be posted later from there, and posts exactly as it
/// would have by session id.
#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_dupcheck_db_a_run_can_be_found_by_its_row_id_as_well() {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new().await;
    fx.link_facility().await;
    let first = fx.run(true, 10).await;
    let second = fx.run(true, 5).await;

    let row_id: Uuid =
        sqlx::query_scalar("SELECT id FROM client_ops.tool_runs WHERE session_id = $1")
            .bind(&second)
            .fetch_one(&fx.superuser)
            .await
            .unwrap();

    // Found by row id, and still the *second* check (its place among the
    // facility's runs does not depend on which id named it).
    let body = body_json(fx.candidates(&row_id.to_string()).await).await;
    assert_eq!(body["step_label"], "2nd Duplicate Check");
    assert_eq!(body["candidates"][0]["task_id"], "c2");

    let response = fx.post(&row_id.to_string(), "c2").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(fx.writes()[0].0, "COMMENT c2");

    // The session id of the other run still works too.
    let body = body_json(fx.candidates(&first).await).await;
    assert_eq!(body["step_label"], "1st Duplicate Check");

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_dupcheck_db_refuses_when_not_linked_or_unknown() {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new().await;
    let saved = fx.run(true, 3).await;
    let unsaved = fx.run(false, 2).await;

    // Not linked yet.
    let response = fx.candidates(&saved).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(response).await["error"],
        "facility_not_linked_to_clickup"
    );

    fx.link_facility().await;

    // File not saved to Dropbox: still offered, but with no link to include.
    let response = fx.candidates(&unsaved).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["file_link_available"], false);

    // No such run.
    let response = fx.candidates(&Uuid::new_v4().to_string()).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_dupcheck_db_posting_comments_assigns_and_completes_the_task() {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new().await;
    fx.link_facility().await;
    let session = fx.run(true, 1).await;

    let response = fx.post(&session, "c1").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["comment"]["ok"], true);
    assert_eq!(body["assignee"]["ok"], true);
    assert_eq!(body["status"]["ok"], true);
    // Fake Dropbox credentials cannot make a share link -> web-path fallback.
    assert_eq!(body["link_kind"], "path");

    let writes = fx.writes();
    assert_eq!(writes.len(), 3);

    assert_eq!(writes[0].0, "COMMENT c1");
    let blocks = writes[0].1["comment"].as_array().unwrap();
    assert_eq!(blocks[0]["text"], "Duplicate check results are ");
    assert_eq!(blocks[1]["text"], "here");
    assert!(blocks[1]["attributes"]["link"]
        .as_str()
        .unwrap()
        .starts_with("https://www.dropbox.com/home/QMS%20Onboarding/"));

    assert_eq!(
        writes[1],
        (
            "PUT c1".to_string(),
            json!({ "assignees": { "add": [42] } })
        )
    );
    assert_eq!(
        writes[2],
        ("PUT c1".to_string(), json!({ "status": "complete" }))
    );

    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM client_ops.audit_log
          WHERE event_type = 'facility_clickup_duplicate_check_posted' AND entity_id = $1",
    )
    .bind(fx.facility_id.to_string())
    .fetch_one(&fx.superuser)
    .await
    .unwrap();
    assert_eq!(audited, 1);

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_dupcheck_db_refuses_without_writing_when_the_task_or_list_is_wrong() {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new().await;
    fx.link_facility().await;
    let session = fx.run(true, 1).await;

    // A task in some other list.
    let response = fx.post(&session, "alien").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await["error"],
        "task_not_in_linked_list"
    );

    // A malformed task id never reaches ClickUp.
    let response = fx.post(&session, "../x").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // A list with no complete status: nothing is changed.
    fx.mock.has_complete_status.store(false, Ordering::SeqCst);
    let response = fx.post(&session, "c1").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(response).await["error"],
        "clickup_no_complete_status"
    );

    assert!(fx.writes().is_empty());

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_dupcheck_db_a_download_only_check_comments_without_a_link() {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new().await;
    fx.link_facility().await;
    let session = fx.run(false, 1).await;

    let response = fx.post(&session, "c1").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["link_kind"], "none");

    let writes = fx.writes();
    assert_eq!(writes.len(), 3);
    assert_eq!(
        writes[0].1["comment"],
        json!([{ "text": "Duplicate check complete." }])
    );

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_dupcheck_db_third_and_later_checks_only_add_a_comment() {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new().await;
    fx.link_facility().await;
    fx.run(true, 30).await;
    fx.run(true, 20).await;
    let third = fx.run(true, 1).await;

    let body = body_json(fx.candidates(&third).await).await;
    assert_eq!(body["comment_only"], true);
    assert_eq!(body["candidates"][0]["task_id"], "c2");

    let response = fx.post(&third, "c2").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["comment"]["ok"], true);
    assert!(body["assignee"].is_null());
    assert!(body["status"].is_null());

    let writes = fx.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, "COMMENT c2");

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_dupcheck_db_prefetch_answers_at_once_and_the_lookup_still_works() {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new().await;

    // Not linked: nothing to warm, still accepted.
    let response = clickup_prefetch::prefetch_facility_tasks(
        State(fx.state.clone()),
        fx.user(),
        Path((fx.company_id, fx.facility_id)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    fx.link_facility().await;
    let session = fx.run(true, 1).await;

    let response = clickup_prefetch::prefetch_facility_tasks(
        State(fx.state.clone()),
        fx.user(),
        Path((fx.company_id, fx.facility_id)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let response = clickup_prefetch::prefetch_hierarchy(State(fx.state.clone()), fx.user()).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    // A lookup right behind the warm-up waits for / reuses it and succeeds.
    let body = body_json(fx.candidates(&session).await).await;
    assert_eq!(body["candidates"][0]["task_id"], "c1");

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
#[serial_test::serial(integration_secrets_encryption_key_env)]
async fn clickup_dupcheck_db_a_share_link_captured_at_save_time_is_the_one_commented() {
    let _ = dotenvy::from_filename(".env.local");
    let fx = Fixture::new().await;
    fx.link_facility().await;
    let session = fx.run(true, 1).await;

    sqlx::query("UPDATE client_ops.tool_runs SET output_dropbox_link = $2 WHERE session_id = $1")
        .bind(&session)
        .bind("https://www.dropbox.com/scl/fi/abc/file.xlsx?rlkey=k&dl=0")
        .execute(&fx.superuser)
        .await
        .unwrap();

    let response = fx.post(&session, "c1").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["link_kind"], "shared");

    let writes = fx.writes();
    assert_eq!(
        writes[0].1["comment"][1]["attributes"]["link"],
        "https://www.dropbox.com/scl/fi/abc/file.xlsx?rlkey=k&dl=0"
    );

    std::env::remove_var("INTEGRATION_SECRETS_ENCRYPTION_KEY");
}
