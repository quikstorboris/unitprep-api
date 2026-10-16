//! Real-database tests for ClickUp Copy's facility dialog (`pairs`,
//! `comments`, `copy`). Every test is `#[ignore]`d -- see `clickup_db_tests`'
//! module doc for how to run them.

use axum::extract::{Json, Path, State};
use axum::http::{HeaderMap, StatusCode};
use uuid::Uuid;

use super::clickup_copy_db_fixture::{status_writes, Fixture};
use super::clickup_db_tests::body_json;
use crate::api::clickup_copy;

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_pairs_tasks_in_every_phase_in_the_templates_order() {
    let f = Fixture::new().await;

    let response = f.pairs(None, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    // The source defaults to the parent; the pointer names the parent.
    assert_eq!(body["source"]["facility_name"], "Main St");
    assert_eq!(body["target"]["facility_name"], "Second St");
    assert_eq!(body["parent"]["facility_name"], "Main St");

    let rows = body["rows"].as_array().unwrap();
    let ids: Vec<&str> = rows
        .iter()
        .map(|row| row["source"]["task_id"].as_str().unwrap())
        .collect();
    // Every phase is offered; a task with no phase is not.
    assert_eq!(ids.len(), 6, "{ids:?}");
    assert!(ids.contains(&"Sstop") && ids.contains(&"Ssched") && ids.contains(&"Strain"));
    assert!(!ids.contains(&"Sloose"));

    // Each row carries its phase and the phase's place in the template's
    // own order, which the page sorts the groups by.
    let order_of = |id: &str| {
        rows.iter()
            .find(|row| row["source"]["task_id"] == id)
            .unwrap()["phase_order"]
            .as_i64()
            .unwrap()
    };
    assert_eq!(
        ["Sfees", "Simport", "Strain", "Ssched", "Sstop"].map(order_of),
        [0, 1, 2, 3, 4]
    );
    let stop = rows
        .iter()
        .find(|row| row["source"]["task_id"] == "Sstop")
        .unwrap();
    assert_eq!(stop["phase"], "Show Stoppers");
    assert_eq!(stop["target"]["task_id"], "Tstop");

    let fees = rows
        .iter()
        .find(|row| row["source"]["task_id"] == "Sfees")
        .unwrap();
    assert_eq!(fees["phase"], "Set Up");
    assert_eq!(fees["target"]["task_id"], "Tfees");
    assert_eq!(fees["source"]["scope"], "corporate");

    let import = rows
        .iter()
        .find(|row| row["source"]["task_id"] == "Simport")
        .unwrap();
    assert_eq!(import["phase"], "Migration");
    assert_eq!(import["target"]["task_id"], "Timport");

    assert_eq!(body["target_tasks"].as_array().unwrap().len(), 6);
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_the_scope_filter_narrows_the_rows() {
    let f = Fixture::new().await;

    let corporate = body_json(f.pairs(None, Some("corporate")).await).await;
    // Corporate: fees, delinquency, open blockers.
    assert_eq!(corporate["rows"].as_array().unwrap().len(), 3);

    let facility = body_json(f.pairs(None, Some("facility")).await).await;
    // Facility: import, training, go-live call (the phase-less task is
    // never offered, whatever its scope).
    assert_eq!(facility["rows"].as_array().unwrap().len(), 3);

    assert_eq!(
        f.pairs(None, Some("nonsense")).await.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_refuses_without_a_source_or_when_the_source_is_the_target_or_unlinked() {
    let f = Fixture::new().await;

    // The target is itself: nothing to copy from.
    assert_eq!(
        f.pairs(Some(f.target), None).await.status(),
        StatusCode::BAD_REQUEST
    );

    // No parent and no explicit source.
    f.set_parent(None).await;
    assert_eq!(f.pairs(None, None).await.status(), StatusCode::CONFLICT);

    // An explicit source with no list linked.
    let unlinked = Fixture::facility(&f.superuser, f.company_id, "Third St").await;
    assert_eq!(
        f.pairs(Some(unlinked), None).await.status(),
        StatusCode::CONFLICT
    );

    // A facility of some other company.
    let other_company: Uuid = sqlx::query_scalar(
        "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
    )
    .bind(format!("Other {}", Uuid::new_v4()))
    .fetch_one(&f.superuser)
    .await
    .unwrap();
    let foreign = Fixture::facility(&f.superuser, other_company, "Foreign").await;
    assert_eq!(
        f.pairs(Some(foreign), None).await.status(),
        StatusCode::BAD_REQUEST
    );

    // An explicit linked sibling works without any parent.
    let sibling = Fixture::facility(&f.superuser, f.company_id, "Fourth St").await;
    f.link(sibling, &f.mock.source_list).await;
    assert_eq!(f.pairs(Some(sibling), None).await.status(), StatusCode::OK);
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_a_row_is_prefilled_with_the_sources_latest_comment() {
    let f = Fixture::new().await;
    f.put_comment("Sfees", "Older note", 100);
    f.put_comment("Sfees", "Latest note", 200);

    let body = body_json(f.comments("Sfees", "Tfees").await).await;

    assert_eq!(body["source_comment"]["text"], "Latest note");
    assert_eq!(body["already_copied"], false);
    assert_eq!(body["pointer_present"], false);
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_flags_a_target_that_already_has_the_comment_or_the_pointer() {
    let f = Fixture::new().await;
    f.put_comment("Sfees", "Fees are configured.", 200);
    // The target already reads the same, differing only in case and
    // punctuation.
    f.put_comment("Tfees", "fees are configured", 300);
    f.put_comment("Timport", "Main task list for this client is Somewhere", 50);
    f.put_comment("Simport", "Tenants imported.", 60);

    let fees = body_json(f.comments("Sfees", "Tfees").await).await;
    assert_eq!(fees["already_copied"], true);
    assert_eq!(fees["pointer_present"], false);

    let import = body_json(f.comments("Simport", "Timport").await).await;
    assert_eq!(import["already_copied"], false);
    assert_eq!(import["pointer_present"], true);
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_posts_the_comment_and_one_pointer_per_task() {
    let f = Fixture::new().await;

    let first = body_json(f.copy(&[("Tfees", "Fees are configured.")]).await).await;
    assert_eq!(first["copied"], 1);
    assert_eq!(first["results"][0]["comment"]["ok"], true);
    assert_eq!(first["results"][0]["pointer"]["state"], "posted");

    // A second copy to the same task adds its comment but not a second pointer.
    let second = body_json(f.copy(&[("Tfees", "Fees changed.")]).await).await;
    assert_eq!(second["results"][0]["pointer"]["state"], "already_present");

    let written = f.written();
    assert_eq!(written.len(), 3, "{written:?}");
    assert_eq!(written[0], "Tfees: Fees are configured.");
    assert!(written[1].starts_with("Tfees: Main task list for this client is List "));
    assert_eq!(written[2], "Tfees: Fees changed.");

    // The pointer's list name is a link to the parent's list.
    let pointer = f.mock.writes.lock().unwrap()[1].1.clone();
    assert!(pointer["comment"][1]["attributes"]["link"].is_string());
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_the_parents_own_tasks_get_no_pointer() {
    let f = Fixture::new().await;
    // Copy *into* the parent from a sibling: the parent is the target.
    let sibling = Fixture::facility(&f.superuser, f.company_id, "Sibling St").await;
    f.link(sibling, &f.mock.target_list).await;

    let response = clickup_copy::copy_comments_to_tasks(
        State(f.state.clone()),
        Fixture::user_for(f.user_id),
        HeaderMap::new(),
        Path((f.company_id, f.parent)),
        Json(clickup_copy::CopyRequest {
            complete_tasks: false,
            source_facility_id: Some(sibling),
            items: vec![clickup_copy::CopyItem {
                // A task in the parent's (source) list.
                target_task_id: "Sfees".to_string(),
                comment: "Done at the sibling.".to_string(),
                source_task_id: None,
            }],
        }),
    )
    .await;

    let body = body_json(response).await;
    assert_eq!(body["results"][0]["comment"]["ok"], true);
    assert_eq!(body["results"][0]["pointer"]["state"], "not_applicable");
    assert_eq!(f.written().len(), 1);
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_without_a_parent_no_pointer_is_posted() {
    let f = Fixture::new().await;
    // An explicit source still works with no parent designated.
    f.set_parent(None).await;

    let response = clickup_copy::copy_comments_to_tasks(
        State(f.state.clone()),
        Fixture::user_for(f.user_id),
        HeaderMap::new(),
        Path((f.company_id, f.target)),
        Json(clickup_copy::CopyRequest {
            complete_tasks: false,
            source_facility_id: Some(f.parent),
            items: vec![clickup_copy::CopyItem {
                target_task_id: "Tfees".to_string(),
                comment: "Hello.".to_string(),
                source_task_id: None,
            }],
        }),
    )
    .await;

    let body = body_json(response).await;
    assert_eq!(body["results"][0]["pointer"]["state"], "not_applicable");
    assert_eq!(f.written(), vec!["Tfees: Hello.".to_string()]);
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_one_denied_row_does_not_stop_the_others() {
    let f = Fixture::new().await;

    let body = body_json(
        f.copy(&[("Tdelinq", "Will be denied."), ("Tfees", "Goes through.")])
            .await,
    )
    .await;

    assert_eq!(body["copied"], 1);
    assert_eq!(body["failed"], 1);
    assert_eq!(body["results"][0]["comment"]["ok"], false);
    assert!(body["results"][0]["comment"]["message"]
        .as_str()
        .unwrap()
        .contains("Access denied"));
    // A failed row gets no pointer; the other row is unaffected.
    assert_eq!(body["results"][0]["pointer"]["state"], "not_applicable");
    assert_eq!(body["results"][1]["comment"]["ok"], true);
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_refuses_bad_requests_without_writing() {
    let f = Fixture::new().await;

    // A task that is not in the facility's list.
    assert_eq!(
        f.copy(&[("elsewhere", "Nope.")]).await.status(),
        StatusCode::BAD_REQUEST
    );
    // An empty comment, and an empty request.
    assert_eq!(
        f.copy(&[("Tfees", "   ")]).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(f.copy(&[]).await.status(), StatusCode::BAD_REQUEST);
    // Too many rows at once.
    let many: Vec<(&str, &str)> = vec![("Tfees", "x"); 31];
    assert_eq!(f.copy(&many).await.status(), StatusCode::BAD_REQUEST);

    assert!(f.written().is_empty());
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_a_dialog_copy_ends_with_a_link_to_the_source_task() {
    let f = Fixture::new().await;

    let response = clickup_copy::copy_comments_to_tasks(
        State(f.state.clone()),
        Fixture::user_for(f.user_id),
        HeaderMap::new(),
        Path((f.company_id, f.target)),
        Json(clickup_copy::CopyRequest {
            complete_tasks: false,
            source_facility_id: Some(f.parent),
            items: vec![clickup_copy::CopyItem {
                target_task_id: "Tfees".to_string(),
                comment: "Fees are set.".to_string(),
                source_task_id: Some("Sfees".to_string()),
            }],
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let writes = f.mock.writes.lock().unwrap().clone();
    let blocks = writes[0].1["comment"].as_array().unwrap();
    assert_eq!(blocks[0]["text"], "Fees are set.");
    // Three line breaks, then the label, then the source task as a link.
    assert_eq!(blocks[1]["text"], "\n\n\nMain tracker task - ");
    assert_eq!(blocks[2]["text"], "CONFIGURE Fees");
    assert_eq!(
        blocks[2]["attributes"]["link"],
        "https://app.clickup.com/t/Sfees"
    );
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn copy_db_the_dialog_completes_the_target_task_only_when_asked() {
    let f = Fixture::new().await;
    let request = |complete_tasks: bool| clickup_copy::CopyRequest {
        source_facility_id: Some(f.parent),
        complete_tasks,
        items: vec![clickup_copy::CopyItem {
            target_task_id: "Tfees".to_string(),
            comment: "Hello.".to_string(),
            source_task_id: None,
        }],
    };
    let post = |complete_tasks: bool| {
        clickup_copy::copy_comments_to_tasks(
            State(f.state.clone()),
            Fixture::user_for(f.user_id),
            HeaderMap::new(),
            Path((f.company_id, f.target)),
            Json(request(complete_tasks)),
        )
    };

    let without = body_json(post(false).await).await;
    assert!(without["results"][0].get("completed").is_none());
    assert!(status_writes(&f).is_empty());

    let with = body_json(post(true).await).await;
    assert_eq!(with["results"][0]["completed"]["ok"], true);
    assert_eq!(status_writes(&f).len(), 1);
}
