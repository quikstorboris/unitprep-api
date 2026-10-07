use super::{apply::*, compare::*, preview::*, rows::*};
use std::sync::Arc;

use crate::api::AppState;
use crate::clients::person_index::ExtractedPerson;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::clients::sync::{company_field_value, facility_field_value};

use crate::clients::intake_mapping::{MappedCompany, MappedFacility};
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;

use crate::api::test_support::{empty_state, onboarding_manager_user, test_user};

fn company_row(legal_name: &str, manually_edited_fields: Vec<&str>) -> CompanyRow {
    CompanyRow {
        id: Uuid::new_v4(),
        ps_intake_run_id: Some("run-highway-20".to_string()),
        legal_name: legal_name.to_string(),
        corporate_email: Some("office@example.com".to_string()),
        corporate_phone: Some("555-000-0000".to_string()),
        corporate_address_street: Some("1 Example St".to_string()),
        corporate_address_city: Some("Example City".to_string()),
        corporate_address_state: Some("IL".to_string()),
        corporate_address_zip: Some("60000".to_string()),
        subdomain: Some("example.qms-email.com".to_string()),
        accepted_payment_methods: Some("Credit Card, ACH".to_string()),
        accounting_basis: Some("Cash".to_string()),
        payment_scheme: Some("Advance".to_string()),
        offers_tenant_insurance_raw: Some("Yes".to_string()),
        insurance_provider: Some("Example Insurance Co".to_string()),
        website_url: Some("https://example.com".to_string()),
        manually_edited_fields: manually_edited_fields
            .into_iter()
            .map(String::from)
            .collect(),
    }
}

fn facility_row(name: &str, phone: &str, manually_edited_fields: Vec<&str>) -> FacilityRow {
    FacilityRow {
        id: Uuid::new_v4(),
        ps_intake_run_id: Some("run-facility".to_string()),
        name: name.to_string(),
        street_address: Some("1 Example St".to_string()),
        city: Some("Example City".to_string()),
        state: Some("IL".to_string()),
        zip: Some("60000".to_string()),
        phone: Some(phone.to_string()),
        email: Some("facility@example.com".to_string()),
        units_count: Some(100),
        primary_storage_offering: Some("Standard Self-Storage".to_string()),
        previous_pms: Some("3rd Party PMS".to_string()),
        access_control_system: Some("Keypad".to_string()),
        go_live_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
        dropbox_folder_url: Some("https://example.com/dropbox".to_string()),
        subdomain: Some("example".to_string()),
        subdomain_exists_in_qms_raw: Some("No".to_string()),
        system_email: Some("system@example.com".to_string()),
        website_url: Some("https://facility.example.com".to_string()),
        manually_edited_fields: manually_edited_fields
            .into_iter()
            .map(String::from)
            .collect(),
    }
}

#[test]
fn classify_company_diff_reports_no_conflicts_when_nothing_is_protected() {
    let row = company_row("Old Legal Name LLC", vec![]);
    let fresh = row.mapped();
    let fresh = MappedCompany {
        legal_name: Some("Prairie Enterprises LLC".to_string()),
        ..fresh
    };
    let comparison = CompanyComparison {
        row,
        fresh: Some(fresh),
    };

    let (safe_count, conflicts) = classify_company_diff(&comparison);

    assert_eq!(safe_count, 1);
    assert!(conflicts.is_empty());
}

#[test]
fn classify_company_diff_surfaces_a_conflict_for_a_protected_field_that_genuinely_differs() {
    let row = company_row("Manually Corrected LLC", vec!["legal_name"]);
    let fresh = row.mapped();
    let fresh = MappedCompany {
        legal_name: Some("Stale PS Legal Name LLC".to_string()),
        ..fresh
    };
    let comparison = CompanyComparison {
        row,
        fresh: Some(fresh),
    };

    let (safe_count, conflicts) = classify_company_diff(&comparison);

    assert_eq!(safe_count, 0);
    assert_eq!(conflicts.len(), 1);
    let conflict = &conflicts[0];
    assert_eq!(conflict.entity_type, "company");
    assert_eq!(conflict.field, "legal_name");
    assert_eq!(
        conflict.current_value.as_deref(),
        Some("Manually Corrected LLC")
    );
    assert_eq!(
        conflict.fresh_value.as_deref(),
        Some("Stale PS Legal Name LLC")
    );
}

#[test]
fn classify_company_diff_reports_nothing_when_no_fresh_data_was_fetched() {
    // A run whose fetch failed (or has no ps_intake_run_id at all) --
    // `fresh: None` -- must never be reported as either a safe update
    // or a conflict; there's nothing to compare against.
    let row = company_row("Some Company LLC", vec!["legal_name"]);
    let comparison = CompanyComparison { row, fresh: None };

    let (safe_count, conflicts) = classify_company_diff(&comparison);

    assert_eq!(safe_count, 0);
    assert!(conflicts.is_empty());
}

#[test]
fn classify_company_diff_does_not_flag_a_protected_field_that_happens_to_already_match() {
    // A field can be manually edited yet coincidentally equal to
    // Process Street's current value -- that's not a conflict, since
    // there is nothing to choose between.
    let row = company_row("Same Value LLC", vec!["legal_name"]);
    let fresh = row.mapped();
    let comparison = CompanyComparison {
        row,
        fresh: Some(fresh),
    };

    let (safe_count, conflicts) = classify_company_diff(&comparison);

    assert_eq!(safe_count, 0);
    assert!(conflicts.is_empty());
}

#[test]
fn classify_facility_diff_reports_no_conflicts_when_nothing_is_protected() {
    let row = facility_row("Highway 20 Self Storage", "555-000-0000", vec![]);
    let fresh = row.mapped();
    let fresh = MappedFacility {
        phone: Some("555-111-1111".to_string()),
        ..fresh
    };
    let comparison = FacilityComparison {
        row,
        fresh: Some(fresh),
    };

    let (safe_count, conflicts) = classify_facility_diff(&comparison);

    assert_eq!(safe_count, 1);
    assert!(conflicts.is_empty());
}

#[test]
fn classify_facility_diff_surfaces_a_conflict_for_a_protected_field_that_genuinely_differs() {
    let row = facility_row("Highway 20 Self Storage", "555-CORRECTED", vec!["phone"]);
    let fresh = row.mapped();
    let fresh = MappedFacility {
        phone: Some("555-STALE".to_string()),
        ..fresh
    };
    let comparison = FacilityComparison {
        row,
        fresh: Some(fresh),
    };

    let (safe_count, conflicts) = classify_facility_diff(&comparison);

    assert_eq!(safe_count, 0);
    assert_eq!(conflicts.len(), 1);
    let conflict = &conflicts[0];
    assert_eq!(conflict.entity_type, "facility");
    assert_eq!(conflict.field, "phone");
    assert_eq!(conflict.current_value.as_deref(), Some("555-CORRECTED"));
    assert_eq!(conflict.fresh_value.as_deref(), Some("555-STALE"));
}

#[test]
fn classify_facility_diff_never_reports_go_live_date() {
    // go_live_date isn't part of the comparison at all -- confirms a
    // facility whose go_live_date differs (which should never happen
    // since nothing ever writes a fresh value into it) still can't
    // surface as a phantom conflict or safe update.
    let row = facility_row("Highway 20 Self Storage", "555-000-0000", vec![]);
    let fresh = MappedFacility {
        go_live_date: chrono::NaiveDate::from_ymd_opt(2026, 12, 31),
        ..row.mapped()
    };
    let comparison = FacilityComparison {
        row,
        fresh: Some(fresh),
    };

    let (safe_count, conflicts) = classify_facility_diff(&comparison);

    assert_eq!(safe_count, 0);
    assert!(conflicts.is_empty());
}

#[test]
fn company_field_value_reads_every_known_field_by_name() {
    let company = company_row("Prairie Enterprises LLC", vec![]).mapped();

    assert_eq!(
        company_field_value(&company, "legal_name").as_deref(),
        Some("Prairie Enterprises LLC")
    );
    assert_eq!(
        company_field_value(&company, "corporate_email").as_deref(),
        Some("office@example.com")
    );
    assert_eq!(company_field_value(&company, "not_a_real_field"), None);
}

#[test]
fn facility_field_value_reads_every_known_field_by_name_including_numeric_ones() {
    let facility = facility_row("Highway 20 Self Storage", "555-000-0000", vec![]).mapped();

    assert_eq!(
        facility_field_value(&facility, "name").as_deref(),
        Some("Highway 20 Self Storage")
    );
    // units_count is Option<i32>, not Option<String> -- confirms it's
    // stringified, not silently dropped as a type mismatch.
    assert_eq!(
        facility_field_value(&facility, "units_count").as_deref(),
        Some("100")
    );
    assert_eq!(facility_field_value(&facility, "not_a_real_field"), None);
}

#[tokio::test]
async fn preview_refuses_insufficient_permission_without_touching_anything() {
    let response = preview_resync(State(empty_state()), test_user(), Path(Uuid::new_v4())).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn preview_reports_not_configured_with_sufficient_permission() {
    let response = preview_resync(
        State(empty_state()),
        onboarding_manager_user(),
        Path(Uuid::new_v4()),
    )
    .await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn apply_refuses_insufficient_permission_without_touching_anything() {
    let response = apply_resync(
        State(empty_state()),
        test_user(),
        Path(Uuid::new_v4()),
        Json(ApplyResyncRequest {
            resolutions: vec![],
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

fn conflict_resolution(
    entity_type: &str,
    entity_id: Uuid,
    field: &str,
    use_fresh: bool,
) -> ConflictResolution {
    ConflictResolution {
        entity_type: entity_type.to_string(),
        entity_id,
        field: field.to_string(),
        use_fresh,
    }
}

#[test]
fn a_field_not_mentioned_in_any_resolution_stays_protected() {
    let stored = vec!["legal_name".to_string()];
    let entity_id = Uuid::new_v4();

    let effective = effective_protected_fields(&stored, &[], "company", entity_id);

    assert_eq!(effective, stored);
}

#[test]
fn a_resolution_with_use_fresh_false_keeps_the_field_protected() {
    let stored = vec!["legal_name".to_string()];
    let entity_id = Uuid::new_v4();
    let resolutions = vec![conflict_resolution(
        "company",
        entity_id,
        "legal_name",
        false,
    )];

    let effective = effective_protected_fields(&stored, &resolutions, "company", entity_id);

    assert_eq!(effective, stored);
}

#[test]
fn a_resolution_with_use_fresh_true_drops_the_field_from_protection() {
    let stored = vec!["legal_name".to_string(), "corporate_phone".to_string()];
    let entity_id = Uuid::new_v4();
    let resolutions = vec![conflict_resolution(
        "company",
        entity_id,
        "legal_name",
        true,
    )];

    let effective = effective_protected_fields(&stored, &resolutions, "company", entity_id);

    assert_eq!(effective, vec!["corporate_phone".to_string()]);
}

#[test]
fn a_resolution_for_a_different_entity_id_does_not_affect_this_one() {
    // Two facilities can each have their own "phone" conflict --
    // resolving one must never accidentally clear the other's.
    let stored = vec!["phone".to_string()];
    let this_facility = Uuid::new_v4();
    let other_facility = Uuid::new_v4();
    let resolutions = vec![conflict_resolution(
        "facility",
        other_facility,
        "phone",
        true,
    )];

    let effective = effective_protected_fields(&stored, &resolutions, "facility", this_facility);

    assert_eq!(effective, stored);
}

#[test]
fn a_resolution_for_a_different_entity_type_with_the_same_id_does_not_affect_this_one() {
    // Belt-and-suspenders: entity_type must be checked too, not just
    // entity_id, even though a company and a facility never actually
    // share a UUID in practice.
    let stored = vec!["subdomain".to_string()];
    let id = Uuid::new_v4();
    let resolutions = vec![conflict_resolution("facility", id, "subdomain", true)];

    let effective = effective_protected_fields(&stored, &resolutions, "company", id);

    assert_eq!(effective, stored);
}

fn fake_comparisons() -> Comparisons {
    (
        CompanyComparison {
            row: company_row("Cached Co", vec![]),
            fresh: None,
        },
        Vec::new(),
        HashMap::new(),
        HashMap::new(),
    )
}

#[test]
fn a_freshly_cached_preview_is_reused() {
    let entry = CachedComparisons {
        computed_at: Instant::now(),
        comparisons: fake_comparisons(),
    };

    assert!(usable_cache_entry(Some(entry)).is_some());
}

#[test]
fn a_cached_preview_older_than_the_ttl_is_not_reused() {
    let entry = CachedComparisons {
        computed_at: Instant::now() - (PREVIEW_CACHE_TTL + Duration::from_secs(1)),
        comparisons: fake_comparisons(),
    };

    assert!(usable_cache_entry(Some(entry)).is_none());
}

#[test]
fn no_cached_preview_at_all_is_not_reused() {
    assert!(usable_cache_entry(None).is_none());
}

/// Real-database test of the apply path's `ps_person_index` rebuild
/// (needs the local `test-db`; see `clickup_db_tests`' module doc for
/// the run command). Seeds a preview into the cache -- so no Process
/// Street call happens -- applies it, and checks the run's index rows
/// were replaced wholesale while another run's rows were left alone.
#[tokio::test]
#[ignore = "needs the local test-db -- see clickup_db_tests' module doc"]
async fn resync_db_apply_replaces_a_runs_person_index_and_leaves_other_runs_alone() {
    use crate::api::clickup_db_tests::{caller, create_user, superuser_pool};
    use crate::process_street::{ProcessStreetClient, ProcessStreetConfig};

    let _ = dotenvy::from_filename(".env.local");
    let superuser = superuser_pool();
    let user_id = create_user(&superuser, "resync").await;
    let run_id = format!("run-{}", Uuid::new_v4());
    let other_run_id = format!("run-other-{}", Uuid::new_v4());
    let legal_name = format!("Resync Co {}", Uuid::new_v4());

    let company_id: Uuid = sqlx::query_scalar(
        "INSERT INTO clients.companies (legal_name, source, ps_intake_run_id)
         VALUES ($1, 'process_street', $2) RETURNING id",
    )
    .bind(&legal_name)
    .bind(&run_id)
    .fetch_one(&superuser)
    .await
    .unwrap();

    // Stale rows apply must replace, and an unrelated run's row it must not touch.
    for (run, name) in [
        (&run_id, "Stale Person"),
        (&other_run_id, "Untouched Person"),
    ] {
        sqlx::query(
            "INSERT INTO clients.ps_person_index
                 (workflow, ps_run_id, run_name, full_name, email, phone, role)
             VALUES ('intake', $1, 'Old Run Name', $2, NULL, NULL, 'owner')",
        )
        .bind(run)
        .bind(name)
        .execute(&superuser)
        .await
        .unwrap();
    }

    let mut row = company_row(&legal_name, vec![]);
    row.id = company_id;
    row.ps_intake_run_id = Some(run_id.clone());

    let person = |name: &str, email: Option<&str>, phone: Option<&str>, role| ExtractedPerson {
        full_name: name.to_string(),
        email: email.map(String::from),
        phone: phone.map(String::from),
        role,
    };
    let mut people_by_run_id = HashMap::new();
    people_by_run_id.insert(
        run_id.clone(),
        vec![
            person("Jane Owner", Some("jane@example.test"), None, "owner"),
            person("Sam Signer", None, Some("555-0101"), "signer"),
            person(
                "Pat Poc",
                Some("pat@example.test"),
                Some("555-0102"),
                "onboarding_poc",
            ),
        ],
    );

    let state = AppState {
        db: crate::db::connect_test(),
        process_street: Some(Arc::new(ProcessStreetClient::new(ProcessStreetConfig {
            api_key: "k1".to_string(),
        }))),
        ..empty_state()
    };
    state.resync_preview_cache.write().insert(
        company_id,
        CachedComparisons {
            computed_at: Instant::now(),
            comparisons: (
                CompanyComparison { row, fresh: None },
                Vec::new(),
                people_by_run_id,
                HashMap::new(),
            ),
        },
    );

    let response = apply_resync(
        State(state),
        caller(user_id, &["onboarding_manager"], &["client_ops.perform"]),
        Path(company_id),
        Json(ApplyResyncRequest {
            resolutions: Vec::new(),
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);

    #[allow(clippy::type_complexity)]
    let rows: Vec<(String, Option<String>, Option<String>, String, String)> = sqlx::query_as(
        "SELECT full_name, email, phone, role, run_name
           FROM clients.ps_person_index
          WHERE workflow = 'intake' AND ps_run_id = $1
          ORDER BY full_name",
    )
    .bind(&run_id)
    .fetch_all(&superuser)
    .await
    .unwrap();

    // The run's rows are exactly the three fresh people (the stale one is
    // gone), each carrying the company's name as run_name (the fallback
    // when there is no ps_sync_state row).
    let expected = |name: &str, email: Option<&str>, phone: Option<&str>, role: &str| {
        (
            name.to_string(),
            email.map(String::from),
            phone.map(String::from),
            role.to_string(),
            legal_name.clone(),
        )
    };
    assert_eq!(
        rows,
        vec![
            expected("Jane Owner", Some("jane@example.test"), None, "owner"),
            expected(
                "Pat Poc",
                Some("pat@example.test"),
                Some("555-0102"),
                "onboarding_poc"
            ),
            expected("Sam Signer", None, Some("555-0101"), "signer"),
        ]
    );

    let untouched: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM clients.ps_person_index
          WHERE ps_run_id = $1 AND full_name = 'Untouched Person'",
    )
    .bind(&other_run_id)
    .fetch_one(&superuser)
    .await
    .unwrap();
    assert_eq!(untouched, 1, "another run's index rows must not be touched");
}
