//! Real-database tests for the Add facility typeahead. Every test is
//! `#[ignore]`d -- local `test-db` only; see `clickup_db_tests`' module doc.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use uuid::Uuid;

use super::clickup_db_tests::{caller, superuser_pool};
use crate::api::clients_add_facility::{search_ps_runs, PsRunSearchQuery};
use crate::api::test_support::empty_state;
use crate::api::AppState;

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn add_facility_db_typeahead_finds_by_title_and_person_and_flags_imported_runs() {
    let _ = dotenvy::from_filename(".env.local");
    let superuser = superuser_pool();
    let tag = Uuid::new_v4().simple().to_string();
    let title_run = format!("title-{tag}");
    let person_run = format!("person-{tag}");
    let imported_run = format!("imported-{tag}");
    let name = format!("Zqx{tag}");

    for (run, title) in [
        (&title_run, format!("{name} Woodland - QMS Onboarding")),
        (&person_run, "Some Other Self Storage".to_string()),
        (&imported_run, format!("{name} Longview - QMS Onboarding")),
    ] {
        sqlx::query(
            "INSERT INTO clients.ps_sync_state (workflow, ps_run_id, run_name, ps_updated_at)
             VALUES ('intake', $1, $2, now())",
        )
        .bind(run)
        .bind(title)
        .execute(&superuser)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO clients.ps_person_index (workflow, ps_run_id, run_name, full_name, email, role)
         VALUES ('intake', $1, 'Some Other Self Storage', $2, $3, 'owner')",
    )
    .bind(&person_run)
    .bind(format!("Pat {name}"))
    .bind(format!("pat-{tag}@example.com"))
    .execute(&superuser)
    .await
    .unwrap();

    let company: Uuid = sqlx::query_scalar(
        "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
    )
    .bind(format!("Owner Co {tag}"))
    .fetch_one(&superuser)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO clients.facilities (company_id, name, source, ps_intake_run_id)
         VALUES ($1, 'Longview', 'manual', $2)",
    )
    .bind(company)
    .bind(&imported_run)
    .execute(&superuser)
    .await
    .unwrap();

    let state = AppState {
        db: crate::db::connect_test(),
        ..empty_state()
    };
    let response = search_ps_runs(
        State(state),
        caller(
            Uuid::new_v4(),
            &["onboarding_manager"],
            &["client_ops.perform"],
        ),
        Query(PsRunSearchQuery { q: name.clone() }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let matches = json["matches"].as_array().unwrap();
    let find = |run: &str| matches.iter().find(|m| m["run_id"] == run).cloned();

    let by_title = find(&title_run).expect("title match");
    assert!(by_title["matched_person"].is_null());
    assert!(by_title["already_in"].is_null());

    let by_person = find(&person_run).expect("person match");
    assert_eq!(by_person["matched_person"], format!("Pat {name}"));

    let imported = find(&imported_run).expect("imported match");
    assert_eq!(imported["already_in"]["company_id"], company.to_string());
}
