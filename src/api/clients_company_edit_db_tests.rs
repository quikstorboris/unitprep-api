//! Real-database tests for the company page's hand edits. Every test is
//! `#[ignore]`d -- local `test-db` only; see `clickup_db_tests`' module doc.

use axum::extract::{Json, Path, State};
use axum::http::{HeaderMap, StatusCode};
use sqlx::PgPool;
use uuid::Uuid;

use super::clickup_db_tests::{caller, superuser_pool};
use crate::api::clients_company_edit::{
    set_implementation_manager, update_company_info, CompanyInfoFields,
    SetImplementationManagerRequest,
};
use crate::api::test_support::empty_state;
use crate::api::AppState;

struct World {
    state: AppState,
    superuser: PgPool,
    company: Uuid,
}

fn fields(name: &str, phone: Option<&str>) -> CompanyInfoFields {
    CompanyInfoFields {
        legal_name: Some(name.to_string()),
        corporate_email: None,
        corporate_phone: phone.map(str::to_string),
        corporate_address_street: None,
        corporate_address_city: None,
        corporate_address_state: None,
        corporate_address_zip: None,
        subdomain: None,
        website_url: None,
        accepted_payment_methods: None,
        accounting_basis: None,
        payment_scheme: None,
        offers_tenant_insurance_raw: None,
        insurance_provider: None,
    }
}

impl World {
    async fn new() -> Self {
        let _ = dotenvy::from_filename(".env.local");
        let superuser = superuser_pool();
        let company: Uuid = sqlx::query_scalar(
            "INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id",
        )
        .bind(format!("Edit Co {}", Uuid::new_v4()))
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

    async fn save(&self, body: CompanyInfoFields) -> StatusCode {
        update_company_info(
            State(self.state.clone()),
            caller(
                Uuid::new_v4(),
                &["onboarding_manager"],
                &["client_ops.perform"],
            ),
            HeaderMap::new(),
            Path(self.company),
            Json(body),
        )
        .await
        .status()
    }

    async fn row(&self) -> (String, Option<String>, Vec<String>) {
        sqlx::query_as(
            "SELECT legal_name, corporate_phone, manually_edited_fields \
               FROM clients.companies WHERE id = $1",
        )
        .bind(self.company)
        .fetch_one(&self.superuser)
        .await
        .unwrap()
    }
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn company_edit_db_saves_and_protects_changed_fields_only() {
    let w = World::new().await;
    let (original_name, _, _) = w.row().await;

    assert_eq!(
        w.save(fields(&original_name, Some("5551234567"))).await,
        StatusCode::NO_CONTENT
    );
    let (name, phone, edited) = w.row().await;
    assert_eq!(name, original_name);
    assert_eq!(phone.as_deref(), Some("5551234567"));
    assert_eq!(edited, vec!["corporate_phone".to_string()]);

    // A second save that renames it adds a field, keeps the first.
    assert_eq!(
        w.save(fields("KH Moss", Some("5551234567"))).await,
        StatusCode::NO_CONTENT
    );
    let (name, _, mut edited) = w.row().await;
    edited.sort();
    assert_eq!(name, "KH Moss");
    assert_eq!(edited, vec!["corporate_phone", "legal_name"]);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn company_edit_db_rejects_a_blank_name_and_an_unknown_company() {
    let w = World::new().await;
    assert_eq!(w.save(fields("  ", None)).await, StatusCode::BAD_REQUEST);

    let response = update_company_info(
        State(w.state.clone()),
        caller(
            Uuid::new_v4(),
            &["onboarding_manager"],
            &["client_ops.perform"],
        ),
        HeaderMap::new(),
        Path(Uuid::new_v4()),
        Json(fields("Nobody", None)),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn company_edit_db_rejects_an_unknown_implementation_manager() {
    let w = World::new().await;
    let response = set_implementation_manager(
        State(w.state.clone()),
        caller(
            Uuid::new_v4(),
            &["onboarding_manager"],
            &["client_ops.perform"],
        ),
        HeaderMap::new(),
        Path(w.company),
        Json(SetImplementationManagerRequest {
            user_id: Some(Uuid::new_v4()),
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Clearing an unassigned manager is a harmless no-op.
    let response = set_implementation_manager(
        State(w.state.clone()),
        caller(
            Uuid::new_v4(),
            &["onboarding_manager"],
            &["client_ops.perform"],
        ),
        HeaderMap::new(),
        Path(w.company),
        Json(SetImplementationManagerRequest { user_id: None }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}
