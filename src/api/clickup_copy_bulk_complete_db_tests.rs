//! Real-database tests for the bulk copy's "also complete the tasks"
//! option. Every test is `#[ignore]`d -- see `clickup_db_tests`' module doc.

use axum::http::StatusCode;
use serde_json::json;

use super::clickup_copy_db_fixture::{status_writes, Fixture};
use super::clickup_db_tests::body_json;

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
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
#[serial_test::serial(integration_secrets_encryption_key_env)]
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
#[serial_test::serial(integration_secrets_encryption_key_env)]
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
#[serial_test::serial(integration_secrets_encryption_key_env)]
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
