//! Real-database test for ClickUp Copy's "Last Synced Project" log. Every
//! test is `#[ignore]`d -- see `clickup_db_tests`' module doc.

use axum::extract::{Path, Query, State};
use uuid::Uuid;

use super::clickup_copy_db_fixture::Fixture;
use super::clickup_db_tests::body_json;
use crate::api::clickup_copy;

/// The facility's "Last Synced Project" log is the copies recorded in the
/// Activity Logs trail against it, newest first, with the source named.
#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_the_sync_log_lists_the_copies_made_onto_a_facility() {
    let f = Fixture::new().await;
    f.copy(&[("Tfees", "Fees are configured.")]).await;
    f.bulk_copy("Fees changed.", &[(f.target, "Tfees")]).await;

    let log = |before_id: Option<Uuid>, limit: Option<i64>| {
        clickup_copy::facility_sync_log(
            State(f.state.clone()),
            Fixture::user_for(f.user_id),
            Path((f.company_id, f.target)),
            Query(clickup_copy::SyncLogQuery { limit, before_id }),
        )
    };

    let body = body_json(log(None, None).await).await;
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{body}");
    assert_eq!(body["has_more"], false);
    // Newest first: the bulk copy, then the dialog copy.
    assert_eq!(entries[0]["bulk"], true);
    assert_eq!(entries[1]["bulk"], false);
    assert_eq!(entries[0]["copied"], 1);
    assert_eq!(entries[0]["failed"], 0);
    assert_eq!(entries[0]["source_facility_id"], f.parent.to_string());
    let parent_name: String =
        sqlx::query_scalar("SELECT name FROM clients.facilities WHERE id = $1")
            .bind(f.parent)
            .fetch_one(&f.superuser)
            .await
            .unwrap();
    assert_eq!(entries[0]["source_facility_name"], parent_name);

    // Paging: one at a time, the second page starts after the first.
    let page = body_json(log(None, Some(1)).await).await;
    assert_eq!(page["entries"].as_array().unwrap().len(), 1);
    assert_eq!(page["has_more"], true);
    let first_id = Uuid::parse_str(page["entries"][0]["id"].as_str().unwrap()).unwrap();
    let next = body_json(log(Some(first_id), Some(1)).await).await;
    assert_eq!(next["entries"][0]["bulk"], false);
    assert_eq!(next["has_more"], false);

    // Another facility of the company has none of these.
    let (other, _) = f.add_facility("Third St").await;
    let body = body_json(
        clickup_copy::facility_sync_log(
            State(f.state.clone()),
            Fixture::user_for(f.user_id),
            Path((f.company_id, other)),
            Query(clickup_copy::SyncLogQuery {
                limit: None,
                before_id: None,
            }),
        )
        .await,
    )
    .await;
    assert!(body["entries"].as_array().unwrap().is_empty());
}
