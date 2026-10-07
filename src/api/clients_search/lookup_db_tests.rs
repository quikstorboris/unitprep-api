//! Real-database tests for the search's local lookups (`lookup::load`) --
//! the part of `GET /clients/search` no hermetic test reaches, since the
//! live Process Street searches come first. Every test is `#[ignore]`d --
//! local `test-db` only; see `clickup_db_tests`' module doc for how to
//! run them.

use chrono::Utc;
use uuid::Uuid;

use super::lookup::load;
use crate::api::clickup_db_tests::{caller, superuser_pool};
use crate::clients::search::SearchResult;

fn search_result(run_id: &str, run_name: &str) -> SearchResult {
    SearchResult {
        run_id: run_id.to_string(),
        run_name: run_name.to_string(),
        status: "Active".to_string(),
        updated_at: Utc::now(),
    }
}

#[tokio::test]
#[serial_test::serial(ps_person_index)]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn search_lookup_db_finds_people_flags_imports_and_adds_the_rest_of_a_facilitys_contacts() {
    let _ = dotenvy::from_filename(".env.local");
    let superuser = superuser_pool();
    let tag = Uuid::new_v4().simple().to_string();
    let title_run = format!("run-title-{tag}");
    let person_run = format!("run-person-{tag}");
    let surname = format!("Zzlookup{}", &tag[..8]);

    // A facility whose own title matched (title_run) with a second, unrelated
    // contact on it, and a run reachable only through a person hit.
    for (run, name, person) in [
        (&title_run, "Title Hit Storage", "Alex Otherperson"),
        (&person_run, "Person Hit Storage", &format!("Pat {surname}")),
    ] {
        sqlx::query(
            "INSERT INTO clients.ps_person_index (workflow, ps_run_id, run_name, full_name, role)
             VALUES ('intake', $1, $2, $3, 'owner')",
        )
        .bind(run)
        .bind(name)
        .bind(person)
        .execute(&superuser)
        .await
        .unwrap();
    }
    // The title-hit run is already imported as a facility.
    let company: Uuid = sqlx::query_scalar(
        "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
    )
    .bind(format!("Lookup Co {tag}"))
    .fetch_one(&superuser)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO clients.facilities (company_id, name, source, ps_intake_run_id)
         VALUES ($1, 'Title Hit Storage', 'manual', $2)",
    )
    .bind(company)
    .bind(&title_run)
    .execute(&superuser)
    .await
    .unwrap();

    let user = caller(Uuid::new_v4(), &["onboarding_manager"], &[]);
    let context = load(
        &crate::db::connect_test(),
        &user,
        &surname,
        &[search_result(&title_run, "Title Hit Storage")],
        &[],
    )
    .await
    .unwrap_or_else(|failure| panic!("lookup failed at: {}", failure.log));

    // The surname matches only the second run's person...
    assert_eq!(context.person_derived.len(), 1);
    assert_eq!(context.person_derived[0].0, person_run);
    // ...and the title-hit facility's other contact is folded in too.
    let names: Vec<&str> = context
        .person_matches
        .iter()
        .map(|p| p.full_name.as_str())
        .collect();
    assert!(names.contains(&"Alex Otherperson"), "got {names:?}");
    assert!(names.iter().any(|n| n.ends_with(&surname)), "got {names:?}");
    assert!(context.already_imported.contains(&title_run));
    assert!(!context.already_imported.contains(&person_run));

    sqlx::query("DELETE FROM clients.ps_person_index WHERE ps_run_id = ANY($1)")
        .bind(vec![title_run, person_run])
        .execute(&superuser)
        .await
        .unwrap();
}
