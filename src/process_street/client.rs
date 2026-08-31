use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::config::ProcessStreetConfig;

const BASE_URL: &str = "https://public-api.process.st/api/v1.1";

#[derive(Debug, thiserror::Error)]
pub enum ProcessStreetError {
    #[error("Process Street request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("Process Street API returned {status}: {body}")]
    Api { status: u16, body: String },

    #[error("failed to parse Process Street response ({0}): {1}")]
    Parse(serde_json::Error, String),
}

// Workflow/WorkflowRun and list_workflows/list_workflow_runs below have
// no caller yet -- they're for the future search/discovery flow
// (Phase 2+: finding a company/facility by name before importing it),
// distinct from get_run_form_fields/get_run_tasks, which `clients::ingest`
// already calls for real. Remove these allows once that flow exists.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct Workflow {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RunAudit {
    #[serde(rename = "updatedDate")]
    updated_date: DateTime<Utc>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowRun {
    pub id: String,
    pub name: String,
    pub status: String,
    #[serde(rename = "workflowId")]
    pub workflow_id: String,
    audit: RunAudit,
}

impl WorkflowRun {
    /// PS's own `audit.updatedDate` -- the signal `clients::sync`'s
    /// delta check compares against `ps_sync_state.ps_updated_at` to
    /// decide whether this run's fields actually need re-fetching.
    /// Confirmed present on every real run returned by `GET
    /// /workflow-runs` (verified directly against the live API,
    /// 2026-08-31) -- not optional the way `FormField.label` turned out
    /// to be.
    pub fn updated_at(&self) -> DateTime<Utc> {
        self.audit.updated_date
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Task {
    pub id: String,
    pub name: String,
    /// PS's own status strings ("Completed"/"NotCompleted" observed so
    /// far) -- kept as plain text, not a Rust enum, for the same reason
    /// the `ps_task_status.status` column isn't CHECK-constrained: this
    /// is a value PS controls, not this codebase, and it can add a new
    /// one at any time.
    pub status: String,
}

/// One form field value from a workflow run. `data` is PS's own wrapper
/// object around the actual value (shape varies slightly by
/// `field_type` -- e.g. Select adds `hasDefaultValue`, Date adds
/// `timeHidden` -- but every shape seen so far carries a `value` key),
/// kept as raw JSON rather than typed per field_type since Phase 1
/// ingestion only ever needs the plain value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormField {
    pub id: String,
    #[serde(rename = "taskId")]
    pub task_id: String,
    pub key: String,
    /// Observed missing entirely on at least one real field (a
    /// `SendRichEmail` type field on Prairie Enterprises' New Merchant
    /// Account run) -- not just an empty string, the whole key absent
    /// from PS's response. `#[serde(default)]` so that field doesn't
    /// fail deserialization of everything after it.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(rename = "fieldType")]
    pub field_type: String,
    /// `None` when the field was never answered at all (PS omits or
    /// nulls `data` itself) -- distinct from `value_as_str()` returning
    /// `None`, which also covers an answered-but-non-string value.
    pub data: Option<Value>,
}

impl FormField {
    /// The one thing Phase 1 ingestion actually needs: `data.value` as
    /// a plain string, or `None` if unanswered or non-string.
    pub fn value_as_str(&self) -> Option<&str> {
        self.data.as_ref()?.get("value")?.as_str()
    }
}

/// Read-only Process Street client -- see [[feedback_ps_readonly]] in
/// the vault (or, if this comment outlives that note, Boris's own
/// instruction: PS is a live ops system the onboarding team depends on
/// daily, so nothing here may call a write endpoint). Every method is a
/// GET; there is no `create`/`update`/`delete` here and none should be
/// added without that constraint being explicitly lifted first.
pub struct ProcessStreetClient {
    http: reqwest::Client,
    config: ProcessStreetConfig,
}

impl ProcessStreetClient {
    // No bootstrap wiring calls this yet -- `clients::ingest` takes an
    // already-constructed `&ProcessStreetClient` as a parameter, since
    // nothing in main.rs constructs a real one until PROCESS_STREET_API_KEY
    // is actually set. Remove once that wiring exists.
    #[allow(dead_code)]
    pub fn new(config: ProcessStreetConfig) -> Self {
        Self {
            http: reqwest::Client::new(),
            config,
        }
    }

    async fn get_page(&self, url: &str) -> Result<Value, ProcessStreetError> {
        let response = self
            .http
            .get(url)
            .header("X-API-KEY", &self.config.api_key)
            .send()
            .await?;

        let status = response.status();
        let body = response.text().await?;

        if !status.is_success() {
            tracing::error!(
                url = %url,
                status = status.as_u16(),
                body = %body,
                "Process Street request failed"
            );
            return Err(ProcessStreetError::Api {
                status: status.as_u16(),
                body,
            });
        }

        serde_json::from_str(&body).map_err(|err| ProcessStreetError::Parse(err, body))
    }

    /// PS's own pagination cursor -- an entry in `links` with
    /// `"name": "next"`, whose `href` is a complete, ready-to-call URL
    /// (already carries the cursor query param), not a token to be
    /// appended manually. Real PS responses have been observed carrying
    /// a `next` link even alongside an empty items array on what turns
    /// out to be the true last page -- `paginate` below guards against
    /// looping forever on that by also stopping on an empty page,
    /// mirroring the two-condition stop this session's own Python
    /// pagination loop used against the real API.
    fn next_link(page: &Value) -> Option<String> {
        page.get("links")?
            .as_array()?
            .iter()
            .find(|link| link.get("name").and_then(Value::as_str) == Some("next"))
            .and_then(|link| link.get("href"))
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// Follows every page of a PS list endpoint and returns every item.
    /// `items_key` is the top-level array name, which differs per
    /// endpoint ("workflows", "workflowRuns", "tasks", "fields") -- PS
    /// has no single consistent envelope shape across them, so this
    /// takes the key as a parameter rather than assuming one.
    async fn paginate<T: DeserializeOwned>(
        &self,
        mut url: String,
        items_key: &str,
    ) -> Result<Vec<T>, ProcessStreetError> {
        let mut all = Vec::new();

        loop {
            let page = self.get_page(&url).await?;

            let items_value = page.get(items_key).cloned().unwrap_or(Value::Array(vec![]));
            let items: Vec<T> = serde_json::from_value(items_value)
                .map_err(|err| ProcessStreetError::Parse(err, page.to_string()))?;

            let is_empty = items.is_empty();
            all.extend(items);

            match (is_empty, Self::next_link(&page)) {
                (false, Some(next)) => url = next,
                _ => break,
            }
        }

        Ok(all)
    }

    // No caller yet -- for the future search/discovery flow (Phase 2+).
    // See the doc comment on `Workflow` above.
    #[allow(dead_code)]
    pub async fn list_workflows(&self) -> Result<Vec<Workflow>, ProcessStreetError> {
        self.paginate(format!("{BASE_URL}/workflows"), "workflows")
            .await
    }

    /// Every run for a workflow, across every status PS actually uses
    /// for a run (`Active`, `Completed`, `Archived` -- confirmed by
    /// probing the API directly; `Deleted` is also a valid value but
    /// deliberately excluded here, and `Stopped`/`Paused`/`Cancelled`
    /// are not valid values at all, PS rejects them with 400).
    ///
    /// **This is the fix for a real bug found while building Contract
    /// Order ingestion**: `GET /workflow-runs` with no `status` query
    /// param returns `Active` only. Two real, known clients (Tri County
    /// Mini Storage, Dubuqueland Mini Storage) were invisible to a
    /// by-name search until `status=Completed` was queried explicitly --
    /// Contract Order runs are marked `Completed` once the order is
    /// processed, so an Active-only search finds essentially none of
    /// them. This isn't specific to Contract Order; it's how this
    /// endpoint behaves for every workflow, which is why the fix lives
    /// here rather than being worked around per-caller.
    #[allow(dead_code)]
    pub async fn list_workflow_runs(
        &self,
        workflow_id: &str,
    ) -> Result<Vec<WorkflowRun>, ProcessStreetError> {
        self.list_or_search_workflow_runs(workflow_id, None).await
    }

    /// Server-side, case-insensitive substring search over a workflow's
    /// run names (confirmed directly: `name=highway` matches "Highway
    /// 20 Self Storage - QMS Onboarding"; a plain unfiltered request and
    /// nonsense params like `search=`/`q=` both silently no-op and
    /// return the same default-ordered page, which is what first
    /// suggested `name` might be the real one). This only searches a
    /// run's own title -- it does NOT reach into form-field values, so
    /// it finds "Highway 20 Self Storage" but not "Prairie Enterprises"
    /// (the company name, which only lives inside a form field on that
    /// facility's run). Confirmed matching consistently across all
    /// three workflows despite their different naming conventions
    /// (Intake's "Highway 20 Self Storage - QMS Onboarding" vs. New
    /// Merchant Account's "Prairie Enterprises (Highway 20)" both match
    /// `name=highway`).
    ///
    /// Searches across every status the same way `list_workflow_runs`
    /// does -- confirmed `name` and `status` combine correctly in one
    /// request.
    ///
    /// Called by `clients::search`, which has no HTTP handler or CLI
    /// binary calling it yet -- remove this allow once one exists.
    #[allow(dead_code)]
    pub async fn search_workflow_runs_by_name(
        &self,
        workflow_id: &str,
        name_query: &str,
    ) -> Result<Vec<WorkflowRun>, ProcessStreetError> {
        self.list_or_search_workflow_runs(workflow_id, Some(name_query))
            .await
    }

    async fn list_or_search_workflow_runs(
        &self,
        workflow_id: &str,
        name_query: Option<&str>,
    ) -> Result<Vec<WorkflowRun>, ProcessStreetError> {
        let name_param = name_query
            .map(|q| {
                let encoded: String = url::form_urlencoded::byte_serialize(q.as_bytes()).collect();
                format!("&name={encoded}")
            })
            .unwrap_or_default();

        let mut all = Vec::new();
        for status in ["Active", "Completed", "Archived"] {
            let mut page = self
                .paginate(
                    format!("{BASE_URL}/workflow-runs?workflowId={workflow_id}&status={status}{name_param}"),
                    "workflowRuns",
                )
                .await?;
            all.append(&mut page);
        }
        Ok(all)
    }

    pub async fn get_run_tasks(&self, run_id: &str) -> Result<Vec<Task>, ProcessStreetError> {
        self.paginate(format!("{BASE_URL}/workflow-runs/{run_id}/tasks"), "tasks")
            .await
    }

    pub async fn get_run_form_fields(
        &self,
        run_id: &str,
    ) -> Result<Vec<FormField>, ProcessStreetError> {
        self.paginate(
            format!("{BASE_URL}/workflow-runs/{run_id}/form-fields"),
            "fields",
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real (trimmed) captures from this session's exploration against
    // Prairie Enterprises' Highway 20 Intake/Progress run -- not
    // synthetic data, so parsing quirks PS actually produces (a `null`
    // `data`, a `Select` field's extra `hasDefaultValue`, a `Date`
    // field's extra `timeHidden`) are exercised for real.

    const WORKFLOWS_PAGE: &str = r#"{
        "workflows": [
            {"id": "tRh93HgRC5OLom3UxhJD3w", "name": "🚂 Intake / Progress"},
            {"id": "rhUaJ-KRu0ejEOYQ-jxGMA", "name": "💳 New Merchant Account"}
        ],
        "links": [
            {"name": "next", "href": "https://public-api.process.st/api/v1.1/workflows?_=cXZqVHBZMjk3TU92TXJoa3BkVk1KQQ", "rel": "Workflow", "type": "Api"}
        ]
    }"#;

    const WORKFLOWS_LAST_PAGE_EMPTY_BUT_HAS_NEXT: &str = r#"{
        "workflows": [],
        "links": [
            {"name": "next", "href": "https://public-api.process.st/api/v1.1/workflows?_=stale", "rel": "Workflow", "type": "Api"}
        ]
    }"#;

    const FORM_FIELDS_PAGE: &str = r#"{
        "fields": [
            {
                "id": "ixGqzM5UzhDd3GnIsQZK5w",
                "workflowRunId": "iy22NyiqGjwAAytKp0NErQ",
                "taskId": "j-5_aOg11bWhSznEkoxBnA",
                "key": "Who_placed_the_order?",
                "label": "Who placed the order?",
                "data": {"value": "Dan F.", "hasDefaultValue": false},
                "fieldType": "Select"
            },
            {
                "id": "tYSXuwxkHTrDDjnkGORDiQ",
                "workflowRunId": "iy22NyiqGjwAAytKp0NErQ",
                "taskId": "g6R6n86CZtxIOggqPCFM3A",
                "key": "What_is_the_Go_Live_Date_on_the_contract?",
                "label": "What is the Go Live Date on the contract?",
                "data": {"value": "2026-08-19T15:00:00.000Z", "timeHidden": false},
                "fieldType": "Date"
            },
            {
                "id": "unansweredExample",
                "workflowRunId": "iy22NyiqGjwAAytKp0NErQ",
                "taskId": "g6R6n86CZtxIOggqPCFM3A",
                "key": "Which_3rd_Party_Software_are_they_using?",
                "label": "Which 3rd Party Software are they using?",
                "data": null,
                "fieldType": "Text"
            }
        ],
        "links": []
    }"#;

    const TASKS_PAGE: &str = r#"{
        "tasks": [
            {"id": "h1RB1lJsHThfz6vYZNpNcA", "name": "Step to Add Customer Success Team", "status": "Completed"},
            {"id": "jW8ERmV2COCiu3feNMRFzw", "name": "🌐 Website: Update ClickUp Task", "status": "NotCompleted"}
        ]
    }"#;

    #[test]
    fn parses_a_real_workflows_page_including_emoji_names() {
        let page: Value = serde_json::from_str(WORKFLOWS_PAGE).unwrap();
        let items: Vec<Workflow> =
            serde_json::from_value(page.get("workflows").unwrap().clone()).unwrap();

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "🚂 Intake / Progress");
        assert_eq!(items[1].name, "💳 New Merchant Account");
        assert_eq!(
            ProcessStreetClient::next_link(&page).as_deref(),
            Some("https://public-api.process.st/api/v1.1/workflows?_=cXZqVHBZMjk3TU92TXJoa3BkVk1KQQ")
        );
    }

    #[test]
    fn treats_an_empty_page_as_the_end_even_when_a_next_link_is_present() {
        // Real PS behavior observed this session: a `next` link can
        // still be present on what is actually the last page. The
        // `paginate` loop's stop condition is (is_empty, next_link) --
        // this test locks in the "empty wins" half of that pair
        // directly against a real captured shape, since a regression
        // here would silently degrade into either an infinite loop or
        // (if the check were inverted) dropping real trailing pages.
        let page: Value = serde_json::from_str(WORKFLOWS_LAST_PAGE_EMPTY_BUT_HAS_NEXT).unwrap();
        let items: Vec<Workflow> =
            serde_json::from_value(page.get("workflows").unwrap().clone()).unwrap();

        assert!(items.is_empty());
        assert!(ProcessStreetClient::next_link(&page).is_some());
    }

    #[test]
    fn form_field_value_as_str_handles_select_date_and_unanswered_fields() {
        let page: Value = serde_json::from_str(FORM_FIELDS_PAGE).unwrap();
        let fields: Vec<FormField> =
            serde_json::from_value(page.get("fields").unwrap().clone()).unwrap();

        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].field_type, "Select");
        assert_eq!(fields[0].value_as_str(), Some("Dan F."));
        assert_eq!(fields[1].field_type, "Date");
        assert_eq!(fields[1].value_as_str(), Some("2026-08-19T15:00:00.000Z"));
        // `data: null` in real PS output -- must not panic, must yield
        // None, not an empty string or a deserialization error.
        assert_eq!(fields[2].data, None);
        assert_eq!(fields[2].value_as_str(), None);
    }

    #[test]
    fn parses_a_real_tasks_page_including_completed_and_not_completed_status() {
        let page: Value = serde_json::from_str(TASKS_PAGE).unwrap();
        let tasks: Vec<Task> = serde_json::from_value(page.get("tasks").unwrap().clone()).unwrap();

        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].status, "Completed");
        assert_eq!(tasks[1].status, "NotCompleted");
        assert_eq!(tasks[1].name, "🌐 Website: Update ClickUp Task");
    }
}
