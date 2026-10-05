//! Real-database tests for the Users tab's Legal Owner fallback: a
//! facility with no Merchant Account owners of its own borrows a sister
//! facility's (see `clients_facility_people::sister_facility_owners`).
//! Every test is `#[ignore]`d -- local `test-db` only; see
//! `clickup_db_tests`' module doc for how to run them.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::clickup_db_tests::{body_json, caller, superuser_pool};
use crate::api::clients_facility_people;
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
        let company = Self::company(&superuser, "Affordable Storage").await;
        Self {
            state: AppState {
                db: crate::db::connect_test(),
                ..empty_state()
            },
            superuser,
            company,
        }
    }

    async fn company(superuser: &PgPool, name: &str) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
        )
        .bind(format!("{name} {}", Uuid::new_v4()))
        .fetch_one(superuser)
        .await
        .unwrap()
    }

    async fn facility(&self, company: Uuid, name: &str) -> Uuid {
        sqlx::query_scalar(
            "INSERT INTO clients.facilities (company_id, name, source) VALUES ($1, $2, 'manual') RETURNING id",
        )
        .bind(company)
        .bind(name)
        .fetch_one(&self.superuser)
        .await
        .unwrap()
    }

    /// A Merchant Account party on `facility`'s form.
    async fn party(&self, facility: Uuid, role: &str, index: i32, name: Option<&str>, email: &str) {
        sqlx::query(
            "INSERT INTO clients.facility_merchant_account_parties
                 (facility_id, party_role, party_index, display_name, email, source)
             VALUES ($1, $2, $3, $4, $5, 'process_street')",
        )
        .bind(facility)
        .bind(role)
        .bind(index)
        .bind(name)
        .bind(email)
        .execute(&self.superuser)
        .await
        .unwrap();
    }

    /// A roster person on `facility` at an access level.
    async fn roster(&self, facility: Uuid, name: &str, email: &str, role: &str) {
        let person: Uuid = sqlx::query_scalar(
            "INSERT INTO clients.people (full_name, email) VALUES ($1, $2) RETURNING id",
        )
        .bind(name)
        .bind(email)
        .fetch_one(&self.superuser)
        .await
        .unwrap();

        sqlx::query(
            "INSERT INTO clients.facility_people (facility_id, person_id, role, source)
             VALUES ($1, $2, $3, 'manual')",
        )
        .bind(facility)
        .bind(person)
        .bind(role)
        .execute(&self.superuser)
        .await
        .unwrap();
    }

    async fn people(&self, company: Uuid, facility: Uuid) -> Value {
        let response = clients_facility_people::get_facility_people(
            State(self.state.clone()),
            caller(Uuid::new_v4(), &["onboarding_manager"], &[]),
            Path((company, facility)),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        body_json(response).await
    }
}

fn legal_owners(people: &Value) -> Vec<String> {
    let mut names: Vec<String> = people["roster"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["legal_owner"] == true)
        .map(|p| p["full_name"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn people_db_a_facility_with_no_form_borrows_a_sisters_owners_and_says_so() {
    let w = World::new().await;
    let westpark = w.facility(w.company, "Westpark").await;
    let copperfield = w.facility(w.company, "Copperfield").await;

    // Only Westpark has a filled Merchant form.
    w.party(westpark, "owner", 1, Some("Beau Ryan"), "beau@example.test")
        .await;
    w.party(westpark, "owner", 2, Some("Brad Ryan"), "brad@example.test")
        .await;

    w.roster(copperfield, "Beau Ryan", "beau@example.test", "owner")
        .await;
    w.roster(copperfield, "Brad Ryan", "brad@example.test", "owner")
        .await;
    w.roster(copperfield, "Ken Withrow", "ken@example.test", "manager")
        .await;

    let people = w.people(w.company, copperfield).await;

    assert_eq!(legal_owners(&people), vec!["Beau Ryan", "Brad Ryan"]);
    assert_eq!(
        people["legal_owner_source"]["facility_id"],
        westpark.to_string()
    );
    assert_eq!(people["legal_owner_source"]["facility_name"], "Westpark");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn people_db_a_facility_with_its_own_owners_keeps_them_and_ignores_sisters() {
    let w = World::new().await;
    let westpark = w.facility(w.company, "Westpark").await;
    let own = w.facility(w.company, "Own Form").await;

    w.party(westpark, "owner", 1, Some("Beau Ryan"), "beau@example.test")
        .await;
    w.party(own, "owner", 1, Some("Pat Local"), "pat@example.test")
        .await;

    w.roster(own, "Beau Ryan", "beau@example.test", "owner")
        .await;
    w.roster(own, "Pat Local", "pat@example.test", "owner")
        .await;

    let people = w.people(w.company, own).await;

    // Its own form wins; the sister's owner is not borrowed.
    assert_eq!(legal_owners(&people), vec!["Pat Local"]);
    assert!(people["legal_owner_source"].is_null());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn people_db_a_form_with_no_named_owner_does_not_count_as_having_owners() {
    let w = World::new().await;
    let westpark = w.facility(w.company, "Westpark").await;
    let signer_only = w.facility(w.company, "Signer Only").await;

    w.party(westpark, "owner", 1, Some("Beau Ryan"), "beau@example.test")
        .await;
    // A form exists but names only a signer and a blank owner slot.
    w.party(
        signer_only,
        "signer",
        1,
        Some("Sam Signer"),
        "sam@example.test",
    )
    .await;
    w.party(signer_only, "owner", 1, None, "blank@example.test")
        .await;

    w.roster(signer_only, "Beau Ryan", "beau@example.test", "owner")
        .await;

    let people = w.people(w.company, signer_only).await;

    assert_eq!(legal_owners(&people), vec!["Beau Ryan"]);
    assert_eq!(people["legal_owner_source"]["facility_name"], "Westpark");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn people_db_the_sister_with_the_most_owners_is_the_one_borrowed_from() {
    let w = World::new().await;
    let one_owner = w.facility(w.company, "Aardvark One Owner").await;
    let two_owners = w.facility(w.company, "Zebra Two Owners").await;
    let target = w.facility(w.company, "Target").await;

    w.party(
        one_owner,
        "owner",
        1,
        Some("Solo Owner"),
        "solo@example.test",
    )
    .await;
    w.party(
        two_owners,
        "owner",
        1,
        Some("Beau Ryan"),
        "beau@example.test",
    )
    .await;
    w.party(
        two_owners,
        "owner",
        2,
        Some("Brad Ryan"),
        "brad@example.test",
    )
    .await;

    w.roster(target, "Beau Ryan", "beau@example.test", "owner")
        .await;

    let people = w.people(w.company, target).await;

    assert_eq!(
        people["legal_owner_source"]["facility_name"],
        "Zebra Two Owners"
    );
    assert_eq!(legal_owners(&people), vec!["Beau Ryan"]);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn people_db_a_borrowed_owner_with_no_roster_row_is_offered_as_missing() {
    let w = World::new().await;
    let westpark = w.facility(w.company, "Westpark").await;
    let target = w.facility(w.company, "Target").await;

    w.party(westpark, "owner", 1, Some("Beau Ryan"), "beau@example.test")
        .await;
    w.roster(target, "Ken Withrow", "ken@example.test", "manager")
        .await;

    let people = w.people(w.company, target).await;

    assert!(legal_owners(&people).is_empty());
    assert_eq!(people["missing_legal_owners"][0]["full_name"], "Beau Ryan");
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn people_db_another_companys_owners_are_never_borrowed() {
    let w = World::new().await;
    let other_company = World::company(&w.superuser, "Someone Else").await;
    let stranger = w.facility(other_company, "Stranger").await;
    let target = w.facility(w.company, "Target").await;

    w.party(stranger, "owner", 1, Some("Beau Ryan"), "beau@example.test")
        .await;
    w.roster(target, "Beau Ryan", "beau@example.test", "owner")
        .await;

    let people = w.people(w.company, target).await;

    assert!(legal_owners(&people).is_empty());
    assert!(people["legal_owner_source"].is_null());
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn people_db_no_owners_anywhere_means_no_checkmarks_and_no_source() {
    let w = World::new().await;
    let target = w.facility(w.company, "Target").await;
    w.roster(target, "Beau Ryan", "beau@example.test", "owner")
        .await;

    let people = w.people(w.company, target).await;

    assert!(legal_owners(&people).is_empty());
    assert!(people["legal_owner_source"].is_null());
}
