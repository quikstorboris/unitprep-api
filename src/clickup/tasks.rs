//! Task-level ClickUp calls: reading a list's tasks and statuses, and the
//! three writes the duplicate-check automation makes (post a comment,
//! add an assignee, set the status). Everything runs with the acting
//! user's own token, like the rest of this module.

use serde::Deserialize;
use serde_json::json;

use super::client::{ClickUpClient, ClickUpError};

/// A list this large would be ~1,000 tasks; stops a misbehaving
/// pagination from looping forever.
const MAX_TASK_PAGES: u32 = 30;

/// Pages requested together: two first (a typical list), then three at a
/// time for the rare bigger one.
const FIRST_PAGE_BATCH: u32 = 2;
const LATER_PAGE_BATCH: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickUpAssignee {
    pub id: String,
    pub username: String,
}

/// One task, reduced to what matching and display need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickUpTask {
    pub id: String,
    pub name: String,
    /// The status's display name in this task's list ("to do", "complete").
    pub status: String,
    /// ClickUp's status class: `open`, `custom`, `done` or `closed`.
    pub status_type: String,
    /// The parent task's id when this is a subtask.
    pub parent_id: Option<String>,
    pub assignees: Vec<ClickUpAssignee>,
    pub url: String,
    pub list_id: Option<String>,
    /// The task's dropdown custom fields that have a value, resolved to
    /// the chosen option (Onboarding Phase and Corp/Fac are dropdowns).
    pub dropdowns: Vec<TaskDropdown>,
}

/// One dropdown custom field's selection on a task.
///
/// ClickUp sends a dropdown's value as the option's *index*, not its name
/// or id (confirmed 2026-10-02, see the vault's ClickUp design log), so it
/// is resolved here through the field's own option list. Matching later
/// goes by `option_id` where it can: emoji in option *names* come back as
/// broken surrogate pairs from the API, which makes names unreliable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDropdown {
    pub field_id: String,
    pub field_name: String,
    pub option_id: String,
    pub option_name: String,
    /// The option's position in the field's own option list. For
    /// Onboarding Phase this is the template's phase order, which is the
    /// order people expect to see the groups in.
    pub option_order: Option<i64>,
}

impl ClickUpTask {
    /// Whether the task is already finished (`done`/`closed` class).
    pub fn is_finished(&self) -> bool {
        matches!(self.status_type.as_str(), "done" | "closed")
    }

    /// The task's selection in the dropdown named `field_name`
    /// (compared by [`compact_label`], so case, spacing and emoji do not
    /// matter).
    pub fn dropdown(&self, field_name: &str) -> Option<&TaskDropdown> {
        let wanted = compact_label(field_name);
        self.dropdowns
            .iter()
            .find(|dropdown| compact_label(&dropdown.field_name) == wanted)
    }
}

/// A label reduced to its lowercase letters and digits, so "Set Up",
/// "SETUP", "🛠 Setup" and "Corp/Fac" vs "corp fac" all compare equal.
/// The templates spell the same group both "Set Up" and "Setup".
pub fn compact_label(label: &str) -> String {
    label
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// One status a list offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickUpStatus {
    pub name: String,
    pub kind: String,
}

#[derive(Deserialize)]
struct TasksPage {
    #[serde(default)]
    tasks: Vec<TaskBody>,
    #[serde(default)]
    last_page: Option<bool>,
}

#[derive(Deserialize)]
struct TaskBody {
    id: String,
    name: String,
    status: Option<StatusBody>,
    parent: Option<String>,
    #[serde(default)]
    assignees: Vec<AssigneeBody>,
    #[serde(default)]
    url: String,
    list: Option<ListRef>,
    #[serde(default)]
    custom_fields: Vec<CustomFieldBody>,
}

#[derive(Deserialize)]
struct CustomFieldBody {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "type", default)]
    kind: String,
    type_config: Option<TypeConfigBody>,
    value: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct TypeConfigBody {
    #[serde(default)]
    options: Vec<OptionBody>,
}

#[derive(Deserialize)]
struct OptionBody {
    id: String,
    #[serde(default)]
    name: String,
    orderindex: Option<serde_json::Number>,
}

impl CustomFieldBody {
    /// The selected option of a dropdown, or `None` for any other field
    /// type, an unset dropdown, or a value matching no option. ClickUp
    /// sends the option's `orderindex` as a number; an option id string
    /// is accepted too, in case a workspace returns that form.
    fn selection(self) -> Option<TaskDropdown> {
        if self.kind != "drop_down" {
            return None;
        }
        let value = self.value?;
        let options = self.type_config?.options;

        let chosen = match &value {
            serde_json::Value::Number(index) => options
                .into_iter()
                .find(|option| option.orderindex.as_ref() == Some(index)),
            serde_json::Value::String(id) => options.into_iter().find(|option| &option.id == id),
            _ => None,
        }?;

        Some(TaskDropdown {
            field_id: self.id,
            field_name: self.name,
            option_id: chosen.id,
            option_name: chosen.name,
            option_order: chosen
                .orderindex
                .as_ref()
                .and_then(serde_json::Number::as_i64),
        })
    }
}

#[derive(Deserialize)]
struct StatusBody {
    status: String,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Deserialize)]
struct AssigneeBody {
    id: serde_json::Number,
    #[serde(default)]
    username: String,
}

#[derive(Deserialize)]
struct ListRef {
    id: String,
}

#[derive(Deserialize)]
struct ListStatuses {
    #[serde(default)]
    statuses: Vec<StatusBody>,
}

impl From<TaskBody> for ClickUpTask {
    fn from(body: TaskBody) -> Self {
        let (status, status_type) = body.status.map(|s| (s.status, s.kind)).unwrap_or_default();

        Self {
            id: body.id,
            name: body.name,
            status,
            status_type,
            parent_id: body.parent,
            assignees: body
                .assignees
                .into_iter()
                .map(|a| ClickUpAssignee {
                    id: a.id.to_string(),
                    username: a.username,
                })
                .collect(),
            url: body.url,
            list_id: body.list.map(|l| l.id),
            dropdowns: body
                .custom_fields
                .into_iter()
                .filter_map(CustomFieldBody::selection)
                .collect(),
        }
    }
}

impl ClickUpClient {
    /// Every task in `list_id`, subtasks and finished tasks included
    /// (the duplicate-check task may already be complete, and a matching
    /// task is often a subtask), following ClickUp's 100-per-page
    /// pagination. Pages are requested a batch at a time rather than one
    /// after another: a real onboarding list is ~110 tasks (two pages),
    /// so the first batch of two covers it in one round trip's time.
    pub async fn list_tasks(
        &self,
        token: &str,
        list_id: &str,
    ) -> Result<Vec<ClickUpTask>, ClickUpError> {
        let mut tasks = Vec::new();
        let mut next_page = 0;
        let mut batch_size = FIRST_PAGE_BATCH;

        while next_page < MAX_TASK_PAGES {
            let pages = next_page..(next_page + batch_size).min(MAX_TASK_PAGES);
            let fetched = futures::future::join_all(pages.map(|page| async move {
                let path =
                    format!("/list/{list_id}/task?page={page}&subtasks=true&include_closed=true");
                self.get_json::<TasksPage>(token, &path).await
            }))
            .await;

            let mut reached_end = false;
            for page in fetched {
                let page = page?;
                reached_end |= page.last_page.unwrap_or(page.tasks.len() < 100);
                tasks.extend(page.tasks.into_iter().map(ClickUpTask::from));
                if reached_end {
                    break;
                }
            }

            if reached_end {
                break;
            }
            next_page += batch_size;
            batch_size = LATER_PAGE_BATCH;
        }

        Ok(tasks)
    }

    /// `GET /task/{id}` -- used to confirm a task the browser names
    /// really lives in the facility's linked list before writing to it.
    pub async fn task(&self, token: &str, task_id: &str) -> Result<ClickUpTask, ClickUpError> {
        let body: TaskBody = self.get_json(token, &format!("/task/{task_id}")).await?;
        Ok(body.into())
    }

    /// The statuses `list_id` offers. Statuses are per list (they differ
    /// between templates), so they are never hard-coded.
    pub async fn list_statuses(
        &self,
        token: &str,
        list_id: &str,
    ) -> Result<Vec<ClickUpStatus>, ClickUpError> {
        let body: ListStatuses = self.get_json(token, &format!("/list/{list_id}")).await?;

        Ok(body
            .statuses
            .into_iter()
            .map(|s| ClickUpStatus {
                name: s.status,
                kind: s.kind,
            })
            .collect())
    }

    /// Posts a comment made of `parts`: `(text, link)` pairs, where a
    /// `Some(url)` makes that stretch of text a hyperlink. Sent once,
    /// never retried (see `send_json`).
    pub async fn add_comment(
        &self,
        token: &str,
        task_id: &str,
        parts: &[(&str, Option<&str>)],
    ) -> Result<(), ClickUpError> {
        let blocks: Vec<serde_json::Value> = parts
            .iter()
            .map(|(text, link)| match link {
                Some(url) => json!({ "text": text, "attributes": { "link": url } }),
                None => json!({ "text": text }),
            })
            .collect();

        self.send_json(
            reqwest::Method::POST,
            token,
            &format!("/task/{task_id}/comment"),
            &json!({ "comment": blocks, "notify_all": false }),
        )
        .await?;

        Ok(())
    }

    /// Adds `user_id` to the task's assignees, leaving anyone already
    /// assigned in place.
    pub async fn add_assignee(
        &self,
        token: &str,
        task_id: &str,
        user_id: i64,
    ) -> Result<(), ClickUpError> {
        self.send_json(
            reqwest::Method::PUT,
            token,
            &format!("/task/{task_id}"),
            &json!({ "assignees": { "add": [user_id] } }),
        )
        .await?;

        Ok(())
    }

    /// Sets the task's status to `status` (a name taken from
    /// [`ClickUpClient::list_statuses`]).
    pub async fn set_status(
        &self,
        token: &str,
        task_id: &str,
        status: &str,
    ) -> Result<(), ClickUpError> {
        self.send_json(
            reqwest::Method::PUT,
            token,
            &format!("/task/{task_id}"),
            &json!({ "status": status }),
        )
        .await?;

        Ok(())
    }
}

/// The status that means "complete" among a list's `statuses`: one
/// literally named complete/completed, else the list's `closed` class,
/// else its `done` class. `None` when the list offers none of those.
pub fn completion_status(statuses: &[ClickUpStatus]) -> Option<&ClickUpStatus> {
    let named = |wanted: &str| {
        statuses
            .iter()
            .find(|s| s.name.trim().eq_ignore_ascii_case(wanted))
    };

    named("complete")
        .or_else(|| named("completed"))
        .or_else(|| statuses.iter().find(|s| s.kind == "closed"))
        .or_else(|| statuses.iter().find(|s| s.kind == "done"))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{
        extract::{Path, Query, State},
        routing::{get, post, put},
        Json, Router,
    };
    use serde_json::Value;

    use super::*;

    fn status(name: &str, kind: &str) -> ClickUpStatus {
        ClickUpStatus {
            name: name.to_string(),
            kind: kind.to_string(),
        }
    }

    /// A task as ClickUp sends it, carrying one Onboarding Phase dropdown
    /// whose value is `value` (an option *index*, as the live API does).
    fn task_with_phase(value: Value) -> ClickUpTask {
        let body: TaskBody = serde_json::from_value(json!({
            "id": "t1", "name": "ADD Recurring Fees",
            "custom_fields": [
                { "id": "f-phase", "name": "Onboarding Phase", "type": "drop_down",
                  "type_config": { "options": [
                      { "id": "o-setup", "name": "Set Up", "orderindex": 0 },
                      { "id": "o-mig", "name": "Migration", "orderindex": 1 }
                  ] },
                  "value": value },
                { "id": "f-text", "name": "Business Name", "type": "short_text", "value": "Acme" }
            ]
        }))
        .unwrap();
        body.into()
    }

    #[test]
    fn a_dropdown_value_is_resolved_through_the_options_order_index() {
        let task = task_with_phase(json!(1));

        let phase = task.dropdown("Onboarding Phase").unwrap();
        assert_eq!(phase.option_id, "o-mig");
        assert_eq!(phase.option_name, "Migration");
        assert_eq!(phase.option_order, Some(1));
        assert_eq!(phase.field_id, "f-phase");
    }

    #[test]
    fn a_dropdown_value_that_is_an_option_id_is_resolved_too() {
        let task = task_with_phase(json!("o-setup"));

        assert_eq!(
            task.dropdown("Onboarding Phase").unwrap().option_name,
            "Set Up"
        );
    }

    #[test]
    fn dropdown_lookup_ignores_case_spacing_and_emoji_in_the_field_name() {
        let task = task_with_phase(json!(0));

        assert!(task.dropdown("onboarding  PHASE").is_some());
        assert!(task.dropdown("🚀 Onboarding Phase").is_some());
        assert!(task.dropdown("Corp/Fac").is_none());
    }

    #[test]
    fn unset_unmatched_and_non_dropdown_fields_resolve_to_nothing() {
        assert!(task_with_phase(Value::Null).dropdowns.is_empty());
        assert!(task_with_phase(json!(9)).dropdowns.is_empty());
        // The short_text field is never reported as a dropdown.
        assert_eq!(task_with_phase(json!(0)).dropdowns.len(), 1);
    }

    #[test]
    fn compact_label_makes_the_two_spellings_of_a_group_equal() {
        assert_eq!(compact_label("Set Up"), compact_label("SETUP"));
        assert_eq!(compact_label("🛠 Setup"), "setup");
        assert_ne!(compact_label("Migration"), compact_label("Set Up"));
    }

    #[test]
    fn a_status_named_complete_wins_over_other_closed_statuses() {
        let statuses = [
            status("to do", "open"),
            status("cancelled", "closed"),
            status("Complete", "closed"),
        ];
        assert_eq!(completion_status(&statuses).unwrap().name, "Complete");
    }

    #[test]
    fn without_a_complete_name_the_closed_class_is_used_then_done() {
        let closed = [status("to do", "open"), status("shipped", "closed")];
        assert_eq!(completion_status(&closed).unwrap().name, "shipped");

        let done = [status("to do", "open"), status("finished", "done")];
        assert_eq!(completion_status(&done).unwrap().name, "finished");

        assert!(completion_status(&[status("to do", "open")]).is_none());
    }

    type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    async fn spawn(seen: Seen) -> ClickUpClient {
        let app = Router::new()
            .route(
                "/list/{id}/task",
                get(|Query(q): Query<std::collections::HashMap<String, String>>| async move {
                    // Two pages: the first says it is not the last.
                    if q.get("page").map(String::as_str) == Some("0") {
                        Json(json!({ "last_page": false, "tasks": [
                            { "id": "t1", "name": "COMPLETE Duplicate Tenant Corrections",
                              "status": { "status": "to do", "type": "open" },
                              "parent": "p1", "assignees": [ { "id": 7, "username": "Ann" } ],
                              "url": "https://app.clickup.com/t/t1", "list": { "id": "L1" } }
                        ] }))
                    } else {
                        Json(json!({ "last_page": true, "tasks": [
                            { "id": "p1", "name": "Parent", "status": { "status": "complete", "type": "closed" },
                              "parent": null, "assignees": [], "url": "u", "list": { "id": "L1" } }
                        ] }))
                    }
                }),
            )
            .route(
                "/list/{id}",
                get(|| async {
                    Json(json!({ "statuses": [
                        { "status": "to do", "type": "open" },
                        { "status": "complete", "type": "closed" }
                    ] }))
                }),
            )
            .route(
                "/task/{id}/comment",
                post(
                    |State(seen): State<Seen>, Path(id): Path<String>, Json(b): Json<Value>| async move {
                        seen.lock().unwrap().push((format!("POST comment {id}"), b));
                        Json(json!({ "id": 1 }))
                    },
                ),
            )
            .route(
                "/task/{id}",
                put(
                    |State(seen): State<Seen>, Path(id): Path<String>, Json(b): Json<Value>| async move {
                        seen.lock().unwrap().push((format!("PUT task {id}"), b));
                        Json(json!({ "id": id }))
                    },
                ),
            )
            .with_state(seen);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        ClickUpClient::new(&format!("http://{addr}"))
    }

    #[tokio::test]
    async fn tasks_are_read_across_pages_with_parent_assignee_and_status() {
        let client = spawn(Seen::default()).await;
        let tasks = client.list_tasks("pk", "L1").await.unwrap();

        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].parent_id.as_deref(), Some("p1"));
        assert_eq!(tasks[0].assignees[0].id, "7");
        assert!(!tasks[0].is_finished());
        assert!(tasks[1].is_finished());
        assert_eq!(tasks[0].list_id.as_deref(), Some("L1"));
    }

    #[tokio::test]
    async fn the_writes_send_the_documented_bodies() {
        let seen = Seen::default();
        let client = spawn(seen.clone()).await;

        client
            .add_comment(
                "pk",
                "t1",
                &[
                    ("Duplicate check results are ", None),
                    ("here", Some("https://x/y")),
                ],
            )
            .await
            .unwrap();
        client.add_assignee("pk", "t1", 42).await.unwrap();
        client.set_status("pk", "t1", "complete").await.unwrap();

        let seen = seen.lock().unwrap();
        assert_eq!(
            seen[0],
            (
                "POST comment t1".to_string(),
                json!({ "comment": [
                    { "text": "Duplicate check results are " },
                    { "text": "here", "attributes": { "link": "https://x/y" } }
                ], "notify_all": false })
            )
        );
        assert_eq!(seen[1].1, json!({ "assignees": { "add": [42] } }));
        assert_eq!(seen[2].1, json!({ "status": "complete" }));
    }

    #[tokio::test]
    async fn list_statuses_are_read_from_the_list() {
        let client = spawn(Seen::default()).await;
        let statuses = client.list_statuses("pk", "L1").await.unwrap();
        assert_eq!(completion_status(&statuses).unwrap().name, "complete");
    }
}
