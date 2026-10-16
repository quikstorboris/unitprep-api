//! Real-database tests for the bulk copy's background jobs. Every test is
//! `#[ignore]`d -- see `clickup_db_tests`' module doc.

use axum::extract::{Json, Path, State};
use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;
use uuid::Uuid;

use super::clickup_copy_db_fixture::Fixture;
use super::clickup_db_tests::{body_json, create_user};
use crate::api::clickup_copy;

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
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
#[serial_test::serial(integration_secrets_encryption_key_env)]
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

/// A small copy normally runs inside the request; the page asks for the
/// background when several tasks copied together add up to too much.
#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_a_small_copy_can_be_sent_to_the_background() {
    let f = Fixture::new().await;

    let response = clickup_copy::bulk_copy(
        State(f.state.clone()),
        Fixture::user_for(f.user_id),
        HeaderMap::new(),
        Path(f.company_id),
        Json(clickup_copy::BulkCopyRequest {
            source_facility_id: None,
            source_task_name: "CONFIGURE Fees".to_string(),
            source_task_id: None,
            complete_tasks: false,
            background: true,
            comment: "Fees are configured.".to_string(),
            destinations: vec![clickup_copy::BulkDestination {
                facility_id: f.target,
                target_task_id: "Tfees".to_string(),
            }],
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let body = body_json(response).await;
    assert_eq!(body["mode"], "job");
    assert_eq!(body["total"], 1);
}
