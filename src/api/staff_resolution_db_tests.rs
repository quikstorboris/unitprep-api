//! Real-database tests for resolving an Intake conductor's email to an OO
//! user (`clients::staff_resolution`): the user directory match, the
//! alias redirect that wins over it, "no match leaves it unassigned",
//! and that an assigned manager is never overwritten. Runs as
//! `app_service`, so RLS applies. Every test is `#[ignore]`d -- see
//! `clickup_db_tests`' module doc for how to run them.

use uuid::Uuid;

use super::clickup_db_tests::{create_user, superuser_pool};
use crate::auth::begin_rls_transaction;
use crate::clients::staff_resolution::{assign_implementation_manager, resolve_user_by_email};
use crate::process_street::FormField;

fn conductor_fields(email: &str) -> Vec<FormField> {
    serde_json::from_value(serde_json::json!([{
        "id": "f1", "taskId": "t1", "key": "Conductor_Email_Address:",
        "label": "Conductor Email Address:", "fieldType": "Email",
        "data": { "value": email, "hasDefaultValue": false }
    }]))
    .expect("a form field")
}

async fn new_company(superuser: &sqlx::PgPool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO clients.companies (legal_name, source) VALUES ('Conductor Test Co', 'manual') RETURNING id",
    )
    .fetch_one(superuser)
    .await
    .unwrap()
}

async fn manager_of(superuser: &sqlx::PgPool, company_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar("SELECT implementation_manager_user_id FROM clients.companies WHERE id = $1")
        .bind(company_id)
        .fetch_one(superuser)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn staff_db_the_conductors_email_resolves_to_the_oo_user_case_insensitively() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();
    let superuser = superuser_pool();
    let manager = create_user(&superuser, "conductor").await;
    let email: String = sqlx::query_scalar("SELECT email::text FROM auth.users WHERE id = $1")
        .bind(manager)
        .fetch_one(&superuser)
        .await
        .unwrap();
    // Any signed-in staff member can run it (company creation does).
    let actor = create_user(&superuser, "actor").await;
    let company = new_company(&superuser).await;

    let mut tx = begin_rls_transaction(&app, actor, &["onboarding_manager".to_string()])
        .await
        .unwrap();
    let assigned =
        assign_implementation_manager(&mut tx, company, &conductor_fields(&email.to_uppercase()))
            .await
            .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(assigned, Some(manager));
    assert_eq!(manager_of(&superuser, company).await, Some(manager));
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn staff_db_an_alias_wins_over_the_directory_and_an_unknown_email_assigns_nobody() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();
    let superuser = superuser_pool();
    let successor = create_user(&superuser, "successor").await;
    let actor = create_user(&superuser, "actor").await;
    let former = format!("former-{}@example.test", Uuid::new_v4());
    sqlx::query(
        "INSERT INTO clients.staff_identity_alias (raw_identifier, resolved_user_id, note)
         VALUES ($1, $2, 'test')",
    )
    .bind(&former)
    .bind(successor)
    .execute(&superuser)
    .await
    .unwrap();
    let company = new_company(&superuser).await;

    let mut tx = begin_rls_transaction(&app, actor, &["onboarding_manager".to_string()])
        .await
        .unwrap();
    assert_eq!(
        resolve_user_by_email(&mut tx, &former).await.unwrap(),
        Some(successor)
    );
    assert_eq!(
        resolve_user_by_email(&mut tx, "nobody-here@example.test")
            .await
            .unwrap(),
        None
    );
    assert_eq!(resolve_user_by_email(&mut tx, "  ").await.unwrap(), None);
    let none = assign_implementation_manager(
        &mut tx,
        company,
        &conductor_fields("nobody-here@example.test"),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(none, None);
    assert_eq!(manager_of(&superuser, company).await, None);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn staff_db_an_assigned_manager_is_never_overwritten() {
    let _ = dotenvy::from_filename(".env.local");
    let app = crate::db::connect_test();
    let superuser = superuser_pool();
    let existing = create_user(&superuser, "existing").await;
    let other = create_user(&superuser, "other").await;
    let other_email: String =
        sqlx::query_scalar("SELECT email::text FROM auth.users WHERE id = $1")
            .bind(other)
            .fetch_one(&superuser)
            .await
            .unwrap();
    let company = new_company(&superuser).await;
    sqlx::query("UPDATE clients.companies SET implementation_manager_user_id = $1 WHERE id = $2")
        .bind(existing)
        .bind(company)
        .execute(&superuser)
        .await
        .unwrap();

    let mut tx = begin_rls_transaction(&app, other, &["onboarding_manager".to_string()])
        .await
        .unwrap();
    assign_implementation_manager(&mut tx, company, &conductor_fields(&other_email))
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(manager_of(&superuser, company).await, Some(existing));
}
