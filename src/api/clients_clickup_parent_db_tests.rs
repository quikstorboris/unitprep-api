//! Real-database tests for the ClickUp parent designation and the "no
//! ClickUp project" waiver. Every test is `#[ignore]`d -- local `test-db`
//! only; see `clickup_db_tests`' module doc for how to run them.

use axum::extract::{Json, Path, State};
use axum::http::{HeaderMap, StatusCode};
use sqlx::PgPool;
use uuid::Uuid;

use super::clickup_db_tests::{caller, create_user, superuser_pool};
use crate::api::clients_clickup_parent::{
    clear_clickup_waiver, set_clickup_parent, waive_clickup_project, SetParentRequest,
};
use crate::api::test_support::empty_state;
use crate::api::AppState;

struct World {
    state: AppState,
    superuser: PgPool,
    /// A real `auth.users` row: the history and waiver columns reference it.
    user_id: Uuid,
    company: Uuid,
    /// Has a ClickUp list linked.
    linked: Uuid,
    /// A second linked facility.
    other_linked: Uuid,
    /// No list linked.
    unlinked: Uuid,
}

impl World {
    async fn new() -> Self {
        let _ = dotenvy::from_filename(".env.local");
        let superuser = superuser_pool();
        let company = Self::company(&superuser).await;
        let user_id = create_user(&superuser, "parent").await;

        let linked = Self::facility(&superuser, company, "Alpha", true).await;
        let other_linked = Self::facility(&superuser, company, "Beta", true).await;
        let unlinked = Self::facility(&superuser, company, "Gamma", false).await;

        Self {
            state: AppState {
                db: crate::db::connect_test(),
                ..empty_state()
            },
            superuser,
            user_id,
            company,
            linked,
            other_linked,
            unlinked,
        }
    }

    async fn company(superuser: &PgPool) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
        )
        .bind(format!("Parent Co {}", Uuid::new_v4()))
        .fetch_one(superuser)
        .await
        .unwrap()
    }

    async fn facility(superuser: &PgPool, company: Uuid, name: &str, linked: bool) -> Uuid {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO clients.facilities (company_id, name, source) VALUES ($1, $2, 'manual') RETURNING id",
        )
        .bind(company)
        .bind(name)
        .fetch_one(superuser)
        .await
        .unwrap();

        if linked {
            sqlx::query(
                "UPDATE clients.facilities SET clickup_list_id = $2, clickup_list_name = $3, \
                 clickup_list_url = $4, clickup_linked_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(format!("L-{id}"))
            .bind(format!("{name} list"))
            .bind("https://app.clickup.com/1/v/li/1")
            .execute(superuser)
            .await
            .unwrap();
        }
        id
    }

    fn manager(&self) -> crate::auth::AuthenticatedUser {
        caller(
            self.user_id,
            &["onboarding_manager"],
            &["client_ops.perform"],
        )
    }

    async fn set_parent(&self, company: Uuid, facility: Option<Uuid>) -> StatusCode {
        set_clickup_parent(
            State(self.state.clone()),
            self.manager(),
            HeaderMap::new(),
            Path(company),
            Json(SetParentRequest {
                facility_id: facility,
            }),
        )
        .await
        .status()
    }

    async fn parent(&self) -> Option<Uuid> {
        sqlx::query_scalar("SELECT clickup_parent_facility_id FROM clients.companies WHERE id = $1")
            .bind(self.company)
            .fetch_one(&self.superuser)
            .await
            .unwrap()
    }

    /// (from name, to name) per history row, oldest first.
    async fn history(&self) -> Vec<(Option<String>, Option<String>)> {
        sqlx::query_as(
            "SELECT from_facility_name, to_facility_name FROM clients.company_clickup_parent_history \
              WHERE company_id = $1 ORDER BY id",
        )
        .bind(self.company)
        .fetch_all(&self.superuser)
        .await
        .unwrap()
    }

    async fn waived_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        sqlx::query_scalar("SELECT clickup_waived_at FROM clients.companies WHERE id = $1")
            .bind(self.company)
            .fetch_one(&self.superuser)
            .await
            .unwrap()
    }

    async fn waive(&self, company: Uuid) -> StatusCode {
        waive_clickup_project(
            State(self.state.clone()),
            self.manager(),
            HeaderMap::new(),
            Path(company),
        )
        .await
        .status()
    }

    async fn clear_waiver(&self, company: Uuid) -> StatusCode {
        clear_clickup_waiver(
            State(self.state.clone()),
            self.manager(),
            HeaderMap::new(),
            Path(company),
        )
        .await
        .status()
    }
}

fn names(from: Option<&str>, to: Option<&str>) -> (Option<String>, Option<String>) {
    (from.map(str::to_string), to.map(str::to_string))
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn parent_db_designating_a_parent_records_it_and_the_first_history_row() {
    let w = World::new().await;

    assert_eq!(
        w.set_parent(w.company, Some(w.linked)).await,
        StatusCode::NO_CONTENT
    );

    assert_eq!(w.parent().await, Some(w.linked));
    assert_eq!(w.history().await, vec![names(None, Some("Alpha"))]);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn parent_db_changing_the_parent_appends_history_in_order() {
    let w = World::new().await;

    assert_eq!(
        w.set_parent(w.company, Some(w.linked)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        w.set_parent(w.company, Some(w.other_linked)).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(w.set_parent(w.company, None).await, StatusCode::NO_CONTENT);

    assert_eq!(w.parent().await, None);
    assert_eq!(
        w.history().await,
        vec![
            names(None, Some("Alpha")),
            names(Some("Alpha"), Some("Beta")),
            names(Some("Beta"), None),
        ]
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn parent_db_setting_the_same_parent_again_is_a_no_op_without_history() {
    let w = World::new().await;
    assert_eq!(
        w.set_parent(w.company, Some(w.linked)).await,
        StatusCode::NO_CONTENT
    );

    assert_eq!(
        w.set_parent(w.company, Some(w.linked)).await,
        StatusCode::NO_CONTENT
    );
    // Clearing a parent that is not set is a no-op too.
    let other = World::company(&w.superuser).await;
    assert_eq!(w.set_parent(other, None).await, StatusCode::NO_CONTENT);

    assert_eq!(w.history().await.len(), 1);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn parent_db_a_facility_without_a_clickup_list_cannot_be_the_parent() {
    let w = World::new().await;

    assert_eq!(
        w.set_parent(w.company, Some(w.unlinked)).await,
        StatusCode::BAD_REQUEST
    );

    assert_eq!(w.parent().await, None);
    assert!(w.history().await.is_empty());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn parent_db_a_facility_of_another_company_cannot_be_the_parent() {
    let w = World::new().await;
    let other_company = World::company(&w.superuser).await;
    let foreign = World::facility(&w.superuser, other_company, "Foreign", true).await;

    assert_eq!(
        w.set_parent(w.company, Some(foreign)).await,
        StatusCode::BAD_REQUEST
    );

    assert_eq!(w.parent().await, None);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn parent_db_an_unknown_company_is_a_404() {
    let w = World::new().await;

    assert_eq!(
        w.set_parent(Uuid::new_v4(), Some(w.linked)).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn parent_db_deleting_the_parent_facility_clears_the_designation_but_keeps_history() {
    let w = World::new().await;
    assert_eq!(
        w.set_parent(w.company, Some(w.linked)).await,
        StatusCode::NO_CONTENT
    );

    sqlx::query("DELETE FROM clients.facilities WHERE id = $1")
        .bind(w.linked)
        .execute(&w.superuser)
        .await
        .unwrap();

    assert_eq!(w.parent().await, None);
    assert_eq!(w.history().await, vec![names(None, Some("Alpha"))]);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn waiver_db_waives_and_clears_a_company() {
    let w = World::new().await;
    assert!(w.waived_at().await.is_none());

    assert_eq!(w.waive(w.company).await, StatusCode::NO_CONTENT);
    assert!(w.waived_at().await.is_some());

    assert_eq!(w.clear_waiver(w.company).await, StatusCode::NO_CONTENT);
    assert!(w.waived_at().await.is_none());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn waiver_db_waiving_twice_keeps_the_first_timestamp() {
    let w = World::new().await;
    assert_eq!(w.waive(w.company).await, StatusCode::NO_CONTENT);
    let first = w.waived_at().await.unwrap();

    assert_eq!(w.waive(w.company).await, StatusCode::NO_CONTENT);

    assert_eq!(w.waived_at().await.unwrap(), first);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn waiver_db_an_unknown_company_is_a_404() {
    let w = World::new().await;

    assert_eq!(w.waive(Uuid::new_v4()).await, StatusCode::NOT_FOUND);
    assert_eq!(w.clear_waiver(Uuid::new_v4()).await, StatusCode::NOT_FOUND);
}
