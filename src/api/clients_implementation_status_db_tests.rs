//! Real-database tests for the "Implementation Completed" toggle. Every
//! test is `#[ignore]`d -- local `test-db` only; see `clickup_db_tests`'
//! module doc for how to run them.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use sqlx::PgPool;
use uuid::Uuid;

use super::clickup_db_tests::{caller, superuser_pool};
use crate::api::clients_implementation_status::{
    mark_implementation_completed, reopen_implementation,
};
use crate::api::test_support::empty_state;
use crate::api::AppState;

struct World {
    state: AppState,
    superuser: PgPool,
    company: Uuid,
}

impl World {
    async fn new() -> Self {
        let _ = dotenvy::from_filename(".env.local");
        let superuser = superuser_pool();
        let company: Uuid = sqlx::query_scalar(
            "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
        )
        .bind(format!("Impl Status Co {}", Uuid::new_v4()))
        .fetch_one(&superuser)
        .await
        .unwrap();
        Self {
            state: AppState {
                db: crate::db::connect_test(),
                ..empty_state()
            },
            superuser,
            company,
        }
    }

    async fn completed_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        sqlx::query_scalar(
            "SELECT implementation_completed_at FROM clients.companies WHERE id = $1",
        )
        .bind(self.company)
        .fetch_one(&self.superuser)
        .await
        .unwrap()
    }

    async fn mark(&self, company: Uuid) -> StatusCode {
        mark_implementation_completed(
            State(self.state.clone()),
            caller(
                Uuid::new_v4(),
                &["onboarding_manager"],
                &["client_ops.perform"],
            ),
            HeaderMap::new(),
            Path(company),
        )
        .await
        .status()
    }

    async fn reopen(&self, company: Uuid) -> StatusCode {
        reopen_implementation(
            State(self.state.clone()),
            caller(
                Uuid::new_v4(),
                &["onboarding_manager"],
                &["client_ops.perform"],
            ),
            HeaderMap::new(),
            Path(company),
        )
        .await
        .status()
    }
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn implementation_db_marks_and_reopens_a_company() {
    let w = World::new().await;
    assert!(w.completed_at().await.is_none());

    assert_eq!(w.mark(w.company).await, StatusCode::NO_CONTENT);
    assert!(w.completed_at().await.is_some());

    assert_eq!(w.reopen(w.company).await, StatusCode::NO_CONTENT);
    assert!(w.completed_at().await.is_none());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn implementation_db_marking_twice_keeps_the_first_timestamp() {
    let w = World::new().await;
    assert_eq!(w.mark(w.company).await, StatusCode::NO_CONTENT);
    let first = w.completed_at().await.unwrap();

    assert_eq!(w.mark(w.company).await, StatusCode::NO_CONTENT);

    assert_eq!(w.completed_at().await.unwrap(), first);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn implementation_db_reopening_a_company_that_was_never_completed_is_a_no_op() {
    let w = World::new().await;

    assert_eq!(w.reopen(w.company).await, StatusCode::NO_CONTENT);

    assert!(w.completed_at().await.is_none());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn implementation_db_an_unknown_company_is_a_404() {
    let w = World::new().await;

    assert_eq!(w.mark(Uuid::new_v4()).await, StatusCode::NOT_FOUND);
    assert_eq!(w.reopen(Uuid::new_v4()).await, StatusCode::NOT_FOUND);
}
