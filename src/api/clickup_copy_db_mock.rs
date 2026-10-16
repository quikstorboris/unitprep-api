//! The mock ClickUp the ClickUp Copy database tests run against: two lists
//! (plus extras for the bulk copy), custom fields and comment state.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Json, Path};
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};

pub(crate) type Comments = Arc<Mutex<HashMap<String, Vec<Value>>>>;
pub(crate) type Writes = Arc<Mutex<Vec<(String, Value)>>>;

#[derive(Clone)]
pub(crate) struct Mock {
    pub(crate) source_list: String,
    pub(crate) target_list: String,
    pub(crate) comments: Comments,
    pub(crate) writes: Writes,
    pub(crate) clock: Arc<AtomicI64>,
    /// Further lists (list id -> task-id prefix) for the bulk copy's extra
    /// facilities; each behaves like the source and target lists.
    pub(crate) extra: Arc<Mutex<HashMap<String, String>>>,
    /// Whether the lists offer a complete status (they do unless a test says not).
    pub(crate) has_complete_status: Arc<std::sync::atomic::AtomicBool>,
}

/// A task as ClickUp sends it, with the Onboarding Phase and Corp/Fac
/// dropdowns carrying their option *index* as the value.
pub(crate) fn task(
    id: &str,
    name: &str,
    parent: Option<&str>,
    list: &str,
    phase: i64,
    scope: i64,
) -> Value {
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
pub(crate) fn list_tasks(list: &str, prefix: &str) -> Value {
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

pub(crate) fn comment_json(id: String, text: &str, date_ms: i64) -> Value {
    json!({ "id": id, "comment_text": text, "date": date_ms.to_string(), "user": { "username": "Ann" } })
}

pub(crate) async fn spawn_clickup(mock: Mock) -> String {
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
