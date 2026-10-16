//! Real-database tests for the client's bulk copy: listing, pairing,
//! prefill, the inline copy and its refusals. Every test is `#[ignore]`d --
//! see `clickup_db_tests`' module doc for how to run them.

use axum::http::StatusCode;
use uuid::Uuid;

use super::clickup_copy_db_fixture::Fixture;
use super::clickup_db_tests::body_json;

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_lists_the_source_tasks_and_the_possible_destinations() {
    let f = Fixture::new().await;
    let (extra, _) = f.add_facility("Third St").await;
    let unlinked = Fixture::facility(&f.superuser, f.company_id, "Fourth St").await;

    let body = body_json(f.bulk_tasks(None).await).await;

    assert_eq!(body["source"]["facility_name"], "Main St");
    assert_eq!(body["parent"]["facility_name"], "Main St");
    let tasks = body["tasks"].as_array().unwrap();
    assert_eq!(
        tasks.len(),
        6,
        "every phase is offered, a phase-less task is not"
    );
    assert_eq!(
        tasks.iter().find(|t| t["task_id"] == "Simport").unwrap()["phase"],
        "Migration"
    );
    let stop = tasks.iter().find(|t| t["task_id"] == "Sstop").unwrap();
    assert_eq!(stop["phase"], "Show Stoppers");
    assert_eq!(stop["phase_order"], 4);

    let destinations: Vec<&str> = body["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["facility_name"].as_str().unwrap())
        .collect();
    assert_eq!(destinations, vec!["Second St", "Third St"]);
    assert!(body["destinations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["facility_id"] == extra.to_string()));
    // A facility with no list is named, but not offered.
    assert_eq!(body["unlinked"][0]["facility_name"], "Fourth St");
    assert_eq!(body["unlinked"][0]["facility_id"], unlinked.to_string());

    let corporate = body_json(f.bulk_tasks(Some("corporate")).await).await;
    assert_eq!(corporate["tasks"].as_array().unwrap().len(), 3);
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_pairs_the_chosen_task_in_every_destination() {
    let f = Fixture::new().await;
    f.add_facility("Third St").await;

    let body = body_json(f.bulk_pairs("Sfees").await).await;

    let destinations = body["destinations"].as_array().unwrap();
    assert_eq!(destinations.len(), 2);
    let second = destinations
        .iter()
        .find(|d| d["facility_name"] == "Second St")
        .unwrap();
    assert_eq!(second["target"]["task_id"], "Tfees");
    assert_eq!(second["tasks"].as_array().unwrap().len(), 6);
    let third = destinations
        .iter()
        .find(|d| d["facility_name"] == "Third St")
        .unwrap();
    assert_eq!(third["target"]["task_id"], "U1fees");
    assert!(third["error"].is_null());

    // A task that is not in the source list.
    assert_eq!(
        f.bulk_pairs("Tfees").await.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_prefills_with_the_source_tasks_latest_comment() {
    let f = Fixture::new().await;
    f.put_comment("Sfees", "Older", 100);
    f.put_comment("Sfees", "Newest", 300);

    let body = body_json(f.bulk_comment("Sfees").await).await;

    assert_eq!(body["source_comment"]["text"], "Newest");
    let none = body_json(f.bulk_comment("Sdelinq").await).await;
    assert!(none["source_comment"].is_null());
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_a_small_copy_runs_inside_the_request() {
    let f = Fixture::new().await;
    let (third, _) = f.add_facility("Third St").await;

    let response = f
        .bulk_copy(
            "Fees are configured.",
            &[(f.target, "Tfees"), (third, "U1fees")],
        )
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert_eq!(body["mode"], "inline");
    assert_eq!(body["copied"], 2);
    assert_eq!(body["failed"], 0);
    assert_eq!(body["results"][0]["facility_name"], "Second St");
    assert_eq!(body["results"][0]["pointer"]["state"], "posted");
    assert_eq!(body["results"][1]["facility_name"], "Third St");

    // The comment and its pointer, on each destination's own task.
    let written = f.written();
    assert_eq!(written.len(), 4, "{written:?}");
    for task in ["Tfees", "U1fees"] {
        assert!(written.contains(&format!("{task}: Fees are configured.")));
        assert!(written
            .iter()
            .any(|w| w.starts_with(&format!("{task}: Main task list for this client is List "))));
    }
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_refuses_bad_requests_without_writing() {
    let f = Fixture::new().await;
    let (third, _) = f.add_facility("Third St").await;
    let unlinked = Fixture::facility(&f.superuser, f.company_id, "Fourth St").await;

    let bad = |response: axum::response::Response| response.status() == StatusCode::BAD_REQUEST;

    // Nothing chosen; an empty comment; the same facility twice.
    assert!(bad(f.bulk_copy("Hi.", &[]).await));
    assert!(bad(f.bulk_copy("  ", &[(third, "U1fees")]).await));
    assert!(bad(f
        .bulk_copy("Hi.", &[(third, "U1fees"), (third, "U1delinq")])
        .await));
    // The source itself, a facility with no list, and a facility of nobody's.
    assert!(bad(f.bulk_copy("Hi.", &[(f.parent, "Sfees")]).await));
    assert!(bad(f.bulk_copy("Hi.", &[(unlinked, "Xfees")]).await));
    assert!(bad(f.bulk_copy("Hi.", &[(Uuid::new_v4(), "Xfees")]).await));
    // A task from another facility's list.
    assert!(bad(f.bulk_copy("Hi.", &[(third, "Tfees")]).await));

    assert!(f.written().is_empty());
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_a_denied_destination_does_not_stop_the_others() {
    let f = Fixture::new().await;
    let (third, _) = f.add_facility("Third St").await;

    let body = body_json(
        f.bulk_copy("Hello.", &[(f.target, "Tdelinq"), (third, "U1fees")])
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
    assert_eq!(body["results"][1]["comment"]["ok"], true);
}

#[tokio::test]
#[serial_test::serial(integration_secrets_encryption_key_env)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn bulk_db_every_copied_comment_ends_with_a_link_to_the_source_task() {
    let f = Fixture::new().await;

    let response = f
        .bulk_copy_from(Some("Sfees"), "Fees are set.", &[(f.target, "Tfees")])
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let writes = f.mock.writes.lock().unwrap().clone();
    let blocks = writes[0].1["comment"].as_array().unwrap();
    assert_eq!(blocks.len(), 3);
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
async fn bulk_db_a_source_task_outside_the_source_list_is_refused_and_nothing_is_posted() {
    let f = Fixture::new().await;

    // Tfees lives in the destination's list, not the source's.
    let response = f
        .bulk_copy_from(Some("Tfees"), "Fees are set.", &[(f.target, "Tfees")])
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await["error"],
        "task_not_in_source_list"
    );
    assert!(f.mock.writes.lock().unwrap().is_empty());
}
