//! Real-database tests for the Onboarding Summary's Elavon "next step":
//! the QMS-credentials cap resolves through the admin-editable task-name
//! mapping and ignores PS-hidden tasks (2026-10-06 template change: new
//! runs show "Document Credentials" and carry the old "Add Credentials
//! to QMS" task hidden). Every test is `#[ignore]`d -- local `test-db`
//! only; see `clickup_db_tests`' module doc for how to run them.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::clickup_db_tests::{body_json, caller, superuser_pool};
use crate::api::clients_onboarding_summary;
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
        .bind(format!("Summary Co {}", Uuid::new_v4()))
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

    /// A facility whose Merchant Account checklist is `tasks`, in PS
    /// order, as `(name, status, hidden)`.
    async fn facility_with_tasks(&self, tasks: &[(&str, &str, bool)]) -> Uuid {
        let facility: Uuid = sqlx::query_scalar(
            "INSERT INTO clients.facilities (company_id, name, source) VALUES ($1, 'Test Facility', 'manual') RETURNING id",
        )
        .bind(self.company)
        .fetch_one(&self.superuser)
        .await
        .unwrap();

        for (index, (name, status, hidden)) in tasks.iter().enumerate() {
            sqlx::query(
                "INSERT INTO clients.ps_task_status
                    (facility_id, workflow, ps_task_id, task_name, status, hidden)
                 VALUES ($1, 'merchant_account', $2, $3, $4, $5)",
            )
            .bind(facility)
            .bind(format!("task-{index}"))
            .bind(name)
            .bind(status)
            .bind(hidden)
            .execute(&self.superuser)
            .await
            .unwrap();
        }
        facility
    }

    async fn summary(&self) -> Value {
        let response = clients_onboarding_summary::get_onboarding_summary(
            State(self.state.clone()),
            caller(Uuid::new_v4(), &["onboarding_manager"], &[]),
            Path(self.company),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        body_json(response).await["facilities"][0].clone()
    }
}

#[tokio::test]
#[serial_test::serial(ps_task_roles)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn summary_db_new_template_waits_on_document_credentials_not_the_hidden_qms_task() {
    let w = World::new().await;
    w.facility_with_tasks(&[
        ("Request Terminal Creation", "Completed", false),
        ("Document Credentials", "NotCompleted", false),
        ("Add Credentials to QMS", "NotCompleted", true),
        ("Twilio Information", "NotCompleted", false),
    ])
    .await;

    let summary = w.summary().await;

    assert_eq!(summary["elavon_next_step"], "Document Credentials");
    assert_eq!(summary["elavon_awaiting_credentials"], true);
}

#[tokio::test]
#[serial_test::serial(ps_task_roles)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn summary_db_completed_document_credentials_caps_the_walk_despite_a_hidden_qms_task() {
    let w = World::new().await;
    w.facility_with_tasks(&[
        ("Request Terminal Creation", "Completed", false),
        ("Document Credentials", "Completed", false),
        ("Add Credentials to QMS", "NotCompleted", true),
        ("Twilio Information", "NotCompleted", false),
    ])
    .await;

    let summary = w.summary().await;

    // Nothing up to the credentials step is outstanding, and the hidden
    // task is neither the cap nor a candidate -- so no next step, and the
    // PS-internal Twilio task after the cap is not surfaced.
    assert!(summary["elavon_next_step"].is_null());
    assert_eq!(summary["elavon_awaiting_credentials"], false);
    assert_eq!(summary["elavon_complete"], true);
}

#[tokio::test]
#[serial_test::serial(ps_task_roles)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn summary_db_complete_follows_the_credentials_step_not_earlier_open_steps() {
    let w = World::new().await;
    w.facility_with_tasks(&[
        ("Facility Info", "Completed", false),
        (
            "Application Signed & Submitted to Elavon",
            "NotCompleted",
            false,
        ),
        ("Document Credentials", "Completed", false),
        ("Add Credentials to QMS", "NotCompleted", true),
    ])
    .await;

    let summary = w.summary().await;

    // The earliest open step is still reported as the status text...
    assert_eq!(
        summary["elavon_next_step"],
        "Application Signed & Submitted to Elavon"
    );
    // ...but "Complete" depends only on the credentials step.
    assert_eq!(summary["elavon_complete"], true);
}

#[tokio::test]
#[serial_test::serial(ps_task_roles)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn summary_db_not_complete_while_the_credentials_step_is_open() {
    let w = World::new().await;
    w.facility_with_tasks(&[
        ("Facility Info", "Completed", false),
        ("Document Credentials", "NotCompleted", false),
    ])
    .await;

    assert_eq!(w.summary().await["elavon_complete"], false);
}

#[tokio::test]
#[serial_test::serial(ps_task_roles)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn summary_db_old_template_still_resolves_through_add_credentials_to_qms() {
    let w = World::new().await;
    w.facility_with_tasks(&[
        ("Facility Info", "Completed", false),
        ("Add Credentials to QMS", "NotCompleted", false),
        ("Twilio Information", "NotCompleted", false),
    ])
    .await;

    let summary = w.summary().await;

    assert_eq!(summary["elavon_next_step"], "Add Credentials to QMS");
    assert_eq!(summary["elavon_awaiting_credentials"], true);
}
