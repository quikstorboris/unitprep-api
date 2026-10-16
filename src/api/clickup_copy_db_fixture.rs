//! The shared fixture of the ClickUp Copy database tests: a company with a
//! parent and a target facility linked to the mock's lists, a connected
//! ClickUp token, and helpers that call each endpoint as that user.

use std::net::SocketAddr;
use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Json, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::clickup_copy_db_mock::{comment_json, spawn_clickup, Comments, Mock, Writes};
use super::clickup_db_tests::{caller, create_user, superuser_pool};
use crate::api::test_support::{empty_state, FakeEnvSource};
use crate::api::{clickup_connection, clickup_copy, AppState};
use crate::auth::AuthenticatedUser;

const ENCRYPTION_KEY: &str = "0000000000000000000000000000000000000000000000000000000000000000";

pub(crate) struct Fixture {
    pub(crate) state: AppState,
    pub(crate) superuser: PgPool,
    pub(crate) mock: Mock,
    pub(crate) user_id: Uuid,
    pub(crate) company_id: Uuid,
    /// The company's parent: linked to the source list.
    pub(crate) parent: Uuid,
    /// The facility being copied to: linked to the target list.
    pub(crate) target: Uuid,
}

impl Fixture {
    pub(crate) async fn new() -> Self {
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

    pub(crate) async fn facility(superuser: &PgPool, company_id: Uuid, name: &str) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO clients.facilities (company_id, name, source) VALUES ($1, $2, 'manual') RETURNING id",
        )
        .bind(company_id)
        .bind(name)
        .fetch_one(superuser)
        .await
        .unwrap()
    }

    pub(crate) async fn link(&self, facility: Uuid, list: &str) {
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
    pub(crate) async fn add_facility(&self, name: &str) -> (Uuid, String) {
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

    pub(crate) async fn set_parent(&self, facility: Option<Uuid>) {
        sqlx::query("UPDATE clients.companies SET clickup_parent_facility_id = $2 WHERE id = $1")
            .bind(self.company_id)
            .bind(facility)
            .execute(&self.superuser)
            .await
            .unwrap();
    }

    pub(crate) fn user_for(user_id: Uuid) -> AuthenticatedUser {
        caller(user_id, &["onboarding_manager"], &["integrations.clickup"])
    }

    pub(crate) fn put_comment(&self, task_id: &str, text: &str, date_ms: i64) {
        self.mock
            .comments
            .lock()
            .unwrap()
            .entry(task_id.to_string())
            .or_default()
            .push(comment_json(format!("seed{date_ms}"), text, date_ms));
    }

    pub(crate) async fn pairs(
        &self,
        source: Option<Uuid>,
        scope: Option<&str>,
    ) -> axum::response::Response {
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

    pub(crate) async fn comments(
        &self,
        source_task: &str,
        target_task: &str,
    ) -> axum::response::Response {
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

    pub(crate) async fn copy(&self, items: &[(&str, &str)]) -> axum::response::Response {
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
    pub(crate) fn written(&self) -> Vec<String> {
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

impl Fixture {
    pub(crate) async fn bulk_tasks(&self, scope: Option<&str>) -> axum::response::Response {
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

    pub(crate) async fn bulk_pairs(&self, source_task: &str) -> axum::response::Response {
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

    pub(crate) async fn bulk_comment(&self, source_task: &str) -> axum::response::Response {
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

    pub(crate) async fn bulk_copy(
        &self,
        comment: &str,
        destinations: &[(Uuid, &str)],
    ) -> axum::response::Response {
        self.bulk_copy_from(None, comment, destinations).await
    }

    /// A bulk copy that names the source task, so the comments end with
    /// the "Main tracker task" link.
    pub(crate) async fn bulk_copy_from(
        &self,
        source_task_id: Option<&str>,
        comment: &str,
        destinations: &[(Uuid, &str)],
    ) -> axum::response::Response {
        self.bulk_copy_full(source_task_id, false, comment, destinations)
            .await
    }

    /// A bulk copy that also asks for the destination tasks to be completed.
    pub(crate) async fn bulk_copy_completing(
        &self,
        comment: &str,
        destinations: &[(Uuid, &str)],
    ) -> axum::response::Response {
        self.bulk_copy_full(None, true, comment, destinations).await
    }

    pub(crate) async fn bulk_copy_full(
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
                background: false,
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

/// Status changes the mock recorded (comment writes have a `comment` key).
pub(crate) fn status_writes(f: &Fixture) -> Vec<(String, Value)> {
    f.mock
        .writes
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, body)| body.get("status").is_some())
        .cloned()
        .collect()
}
