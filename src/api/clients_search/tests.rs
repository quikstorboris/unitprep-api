use std::collections::HashMap;

use chrono::DateTime;

use axum::extract::{Query, State};

use super::dto::{MatchedVia, PersonMatch};
use super::matching::{
    derive_facilities_from_person_matches, facility_matches_for, similar_facility_names_for,
    DisplayLookups, FacilityHit, MaDisplayInfo,
};
use super::{search_clients, SearchClientsQuery};
use crate::api::test_support::{empty_state, test_user};
use crate::clients::merchant_account_correlation::Correlation;
use axum::http::StatusCode;

#[tokio::test]
async fn blank_query_is_rejected_without_touching_anything() {
    let response = search_clients(
        State(empty_state()),
        test_user(),
        Query(SearchClientsQuery {
            q: "   ".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn missing_process_street_config_returns_service_unavailable() {
    // empty_state() carries process_street: None -- the same
    // "not configured" state a real deployment without
    // PROCESS_STREET_API_KEY set would have.
    let response = search_clients(
        State(empty_state()),
        test_user(),
        Query(SearchClientsQuery {
            q: "highway".to_string(),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

fn person_match(
    workflow: &str,
    run_id: &str,
    run_name: &str,
    full_name: &str,
    role: &str,
) -> PersonMatch {
    PersonMatch {
        workflow: workflow.to_string(),
        ps_run_id: run_id.to_string(),
        run_name: run_name.to_string(),
        full_name: full_name.to_string(),
        email: None,
        phone: None,
        role: role.to_string(),
    }
}

#[test]
fn person_hit_on_a_new_run_surfaces_as_a_derived_facility() {
    // Searching "Prairie Enterprises" hits nothing in the Intake
    // titles themselves, but the shared owner shows up on
    // Carpentersville's run -- that's the real case this exists for.
    let matches = vec![person_match(
        "intake",
        "run-carpentersville",
        "Carpentersville Self Storage - QMS Onboarding",
        "Judy Armstrong",
        "owner",
    )];
    let literal_run_ids = std::collections::HashSet::new();

    let derived = derive_facilities_from_person_matches(&matches, &literal_run_ids);

    assert_eq!(
        derived,
        vec![(
            "run-carpentersville".to_string(),
            "Carpentersville Self Storage - QMS Onboarding".to_string(),
            "Judy Armstrong".to_string(),
            "owner".to_string(),
        )]
    );
}

#[test]
fn a_run_already_found_by_literal_title_is_not_duplicated() {
    let matches = vec![person_match(
        "intake",
        "run-highway-20",
        "Highway 20 Self Storage - QMS Onboarding",
        "Kyle Lindley",
        "owner",
    )];
    let mut literal_run_ids = std::collections::HashSet::new();
    literal_run_ids.insert("run-highway-20");

    let derived = derive_facilities_from_person_matches(&matches, &literal_run_ids);

    assert!(derived.is_empty());
}

#[test]
fn non_intake_person_hits_never_become_facility_matches() {
    // A Merchant Account or Contract Order run id isn't a facility
    // identity -- only its own Intake run is.
    let matches = vec![person_match(
        "merchant_account",
        "run-merchant-account",
        "Prairie Enterprises (Highway 20)",
        "Kyle Lindley",
        "signer",
    )];
    let literal_run_ids = std::collections::HashSet::new();

    let derived = derive_facilities_from_person_matches(&matches, &literal_run_ids);

    assert!(derived.is_empty());
}

#[test]
fn the_same_run_hit_by_multiple_people_is_only_listed_once() {
    // All three of Prairie's owners appear on Carpentersville's own
    // run -- the UI needs one derived facility row, not three.
    let matches = vec![
        person_match(
            "intake",
            "run-carpentersville",
            "Carpentersville Self Storage - QMS Onboarding",
            "Judy Armstrong",
            "owner",
        ),
        person_match(
            "intake",
            "run-carpentersville",
            "Carpentersville Self Storage - QMS Onboarding",
            "Kyle Lindley",
            "owner",
        ),
    ];
    let literal_run_ids = std::collections::HashSet::new();

    let derived = derive_facilities_from_person_matches(&matches, &literal_run_ids);

    assert_eq!(derived.len(), 1);
    assert_eq!(
        derived[0].2, "Judy Armstrong",
        "first match in order wins as the shown reason"
    );
}

fn ma_display_with_name(company_name: &str) -> MaDisplayInfo {
    MaDisplayInfo {
        company_name: Some(company_name.to_string()),
        ..Default::default()
    }
}

fn no_correlation_context() -> (
    HashMap<String, MaDisplayInfo>,
    HashMap<String, chrono::DateTime<chrono::Utc>>,
) {
    (HashMap::new(), HashMap::new())
}

#[test]
fn no_correlation_produces_one_row_with_no_company_name_or_duplicate() {
    let (ma_display, ma_updated_at) = no_correlation_context();

    let matches = facility_matches_for(
        FacilityHit {
            run_id: "run-solo".to_string(),
            run_name: "Solo Storage - QMS Onboarding".to_string(),
            status: Some("Active".to_string()),
            matched_via: MatchedVia::Name,
            already_imported: false,
            last_activity_at: None,
        },
        None,
        &DisplayLookups {
            ma_display: &ma_display,
            merchant_account_updated_at: &ma_updated_at,
        },
    );

    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].company_name, None);
    assert!(matches[0].duplicate.is_none());
}

#[test]
fn an_unambiguous_correlation_produces_one_row_with_a_resolved_company_name() {
    let mut ma_display = HashMap::new();
    ma_display.insert(
        "ma-highway-20".to_string(),
        ma_display_with_name("Prairie Enterprises LLC"),
    );
    let ma_updated_at = HashMap::new();
    let correlation = Correlation::Unambiguous("ma-highway-20".to_string());

    let matches = facility_matches_for(
        FacilityHit {
            run_id: "run-highway-20".to_string(),
            run_name: "Highway 20 Self Storage - QMS Onboarding".to_string(),
            status: Some("Active".to_string()),
            matched_via: MatchedVia::Name,
            already_imported: false,
            last_activity_at: None,
        },
        Some(&correlation),
        &DisplayLookups {
            ma_display: &ma_display,
            merchant_account_updated_at: &ma_updated_at,
        },
    );

    assert_eq!(matches.len(), 1);
    assert_eq!(
        matches[0].company_name.as_deref(),
        Some("Prairie Enterprises LLC")
    );
    assert!(matches[0].duplicate.is_none());
}

#[test]
fn an_ambiguous_correlation_produces_one_row_per_candidate_sharing_the_same_facility_identity() {
    // The real Carpentersville case: two distinct, identically
    // titled Merchant Account runs, each resolving to its own
    // (possibly differing) suggested company name.
    let mut ma_display = HashMap::new();
    ma_display.insert(
        "ma-carpentersville-1".to_string(),
        ma_display_with_name("Prairie Enterprises LLC"),
    );
    ma_display.insert(
        "ma-carpentersville-2".to_string(),
        ma_display_with_name("Carpentersville Self Storage"),
    );
    let mut ma_updated_at = HashMap::new();
    let older = chrono::DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
        .unwrap()
        .to_utc();
    let newer = chrono::DateTime::parse_from_rfc3339("2026-08-30T00:00:00Z")
        .unwrap()
        .to_utc();
    ma_updated_at.insert("ma-carpentersville-1".to_string(), newer);
    ma_updated_at.insert("ma-carpentersville-2".to_string(), older);
    let correlation = Correlation::Ambiguous(vec![
        "ma-carpentersville-1".to_string(),
        "ma-carpentersville-2".to_string(),
    ]);

    let matches = facility_matches_for(
        FacilityHit {
            run_id: "run-carpentersville".to_string(),
            run_name: "Carpentersville Self Storage - QMS Onboarding".to_string(),
            status: Some("Active".to_string()),
            matched_via: MatchedVia::Name,
            already_imported: false,
            last_activity_at: None,
        },
        Some(&correlation),
        &DisplayLookups {
            ma_display: &ma_display,
            merchant_account_updated_at: &ma_updated_at,
        },
    );

    assert_eq!(matches.len(), 2);
    // Every row shares the same real facility's identity -- the
    // frontend brackets on this.
    assert!(matches.iter().all(|m| m.run_id == "run-carpentersville"));

    let candidate_1 = matches
        .iter()
        .find(|m| m.duplicate.as_ref().unwrap().merchant_account_run_id == "ma-carpentersville-1")
        .expect("candidate 1 present");
    assert_eq!(
        candidate_1.company_name.as_deref(),
        Some("Prairie Enterprises LLC")
    );
    assert_eq!(
        candidate_1
            .duplicate
            .as_ref()
            .unwrap()
            .merchant_account_updated_at,
        newer
    );

    let candidate_2 = matches
        .iter()
        .find(|m| m.duplicate.as_ref().unwrap().merchant_account_run_id == "ma-carpentersville-2")
        .expect("candidate 2 present");
    assert_eq!(
        candidate_2.company_name.as_deref(),
        Some("Carpentersville Self Storage")
    );
    assert_eq!(
        candidate_2
            .duplicate
            .as_ref()
            .unwrap()
            .merchant_account_updated_at,
        older
    );
}

#[test]
fn ambiguous_candidates_with_matching_addresses_report_addresses_agree_true() {
    // The real Carpentersville shape: a genuine duplicate submission
    // of the same application, same real address both times.
    let mut ma_display = HashMap::new();
    ma_display.insert(
        "ma-1".to_string(),
        MaDisplayInfo {
            business_address: Some("123 Main St, Springfield, IL 62704".to_string()),
            ..Default::default()
        },
    );
    ma_display.insert(
        "ma-2".to_string(),
        MaDisplayInfo {
            business_address: Some("123 Main Street, Springfield, IL 62704".to_string()),
            ..Default::default()
        },
    );
    let ma_updated_at = HashMap::from([
        ("ma-1".to_string(), DateTime::UNIX_EPOCH),
        ("ma-2".to_string(), DateTime::UNIX_EPOCH),
    ]);
    let correlation = Correlation::Ambiguous(vec!["ma-1".to_string(), "ma-2".to_string()]);

    let matches = facility_matches_for(
        FacilityHit {
            run_id: "run-x".to_string(),
            run_name: "Some Facility".to_string(),
            status: None,
            matched_via: MatchedVia::Name,
            already_imported: false,
            last_activity_at: None,
        },
        Some(&correlation),
        &DisplayLookups {
            ma_display: &ma_display,
            merchant_account_updated_at: &ma_updated_at,
        },
    );

    assert!(matches
        .iter()
        .all(|m| m.duplicate.as_ref().unwrap().addresses_agree == Some(true)));
}

#[test]
fn ambiguous_candidates_with_different_addresses_report_addresses_agree_false() {
    // The real Knapp's Self Stor of Milton Freewater / "Milton Self
    // Storage" shape -- two different real businesses, two
    // different real addresses.
    let mut ma_display = HashMap::new();
    ma_display.insert(
        "ma-knapps".to_string(),
        MaDisplayInfo {
            business_address: Some("84097 Hwy 11, Milton Freewater, OR 97862".to_string()),
            ..Default::default()
        },
    );
    ma_display.insert(
        "ma-milton-self-storage".to_string(),
        MaDisplayInfo {
            business_address: Some("500 Elm St, Springfield, IL 62704".to_string()),
            ..Default::default()
        },
    );
    let ma_updated_at = HashMap::from([
        ("ma-knapps".to_string(), DateTime::UNIX_EPOCH),
        ("ma-milton-self-storage".to_string(), DateTime::UNIX_EPOCH),
    ]);
    let correlation = Correlation::Ambiguous(vec![
        "ma-knapps".to_string(),
        "ma-milton-self-storage".to_string(),
    ]);

    let matches = facility_matches_for(
        FacilityHit {
            run_id: "run-x".to_string(),
            run_name: "Some Facility".to_string(),
            status: None,
            matched_via: MatchedVia::Name,
            already_imported: false,
            last_activity_at: None,
        },
        Some(&correlation),
        &DisplayLookups {
            ma_display: &ma_display,
            merchant_account_updated_at: &ma_updated_at,
        },
    );

    assert!(matches
        .iter()
        .all(|m| m.duplicate.as_ref().unwrap().addresses_agree == Some(false)));
}

#[test]
fn ambiguous_candidates_report_no_addresses_agreement_when_fewer_than_two_answered() {
    let mut ma_display = HashMap::new();
    ma_display.insert(
        "ma-1".to_string(),
        MaDisplayInfo {
            business_address: Some("123 Main St".to_string()),
            ..Default::default()
        },
    );
    ma_display.insert("ma-2".to_string(), MaDisplayInfo::default());
    let ma_updated_at = HashMap::from([
        ("ma-1".to_string(), DateTime::UNIX_EPOCH),
        ("ma-2".to_string(), DateTime::UNIX_EPOCH),
    ]);
    let correlation = Correlation::Ambiguous(vec!["ma-1".to_string(), "ma-2".to_string()]);

    let matches = facility_matches_for(
        FacilityHit {
            run_id: "run-x".to_string(),
            run_name: "Some Facility".to_string(),
            status: None,
            matched_via: MatchedVia::Name,
            already_imported: false,
            last_activity_at: None,
        },
        Some(&correlation),
        &DisplayLookups {
            ma_display: &ma_display,
            merchant_account_updated_at: &ma_updated_at,
        },
    );

    assert!(matches
        .iter()
        .all(|m| m.duplicate.as_ref().unwrap().addresses_agree.is_none()));
}

#[test]
fn ein_last_4_and_business_address_carry_through_to_the_duplicate_candidate() {
    let mut ma_display = HashMap::new();
    ma_display.insert(
        "ma-1".to_string(),
        MaDisplayInfo {
            ein_last_4: Some("•••••1111".to_string()),
            business_address: Some("123 Main St".to_string()),
            ..Default::default()
        },
    );
    ma_display.insert("ma-2".to_string(), MaDisplayInfo::default());
    let ma_updated_at = HashMap::from([
        ("ma-1".to_string(), DateTime::UNIX_EPOCH),
        ("ma-2".to_string(), DateTime::UNIX_EPOCH),
    ]);
    let correlation = Correlation::Ambiguous(vec!["ma-1".to_string(), "ma-2".to_string()]);

    let matches = facility_matches_for(
        FacilityHit {
            run_id: "run-x".to_string(),
            run_name: "Some Facility".to_string(),
            status: None,
            matched_via: MatchedVia::Name,
            already_imported: false,
            last_activity_at: None,
        },
        Some(&correlation),
        &DisplayLookups {
            ma_display: &ma_display,
            merchant_account_updated_at: &ma_updated_at,
        },
    );

    let candidate_1 = matches
        .iter()
        .find(|m| m.duplicate.as_ref().unwrap().merchant_account_run_id == "ma-1")
        .expect("candidate 1 present");
    assert_eq!(
        candidate_1
            .duplicate
            .as_ref()
            .unwrap()
            .ein_last_4
            .as_deref(),
        Some("•••••1111")
    );
    assert_eq!(
        candidate_1
            .duplicate
            .as_ref()
            .unwrap()
            .business_address
            .as_deref(),
        Some("123 Main St")
    );

    let candidate_2 = matches
        .iter()
        .find(|m| m.duplicate.as_ref().unwrap().merchant_account_run_id == "ma-2")
        .expect("candidate 2 present");
    assert_eq!(candidate_2.duplicate.as_ref().unwrap().ein_last_4, None);
    assert_eq!(
        candidate_2.duplicate.as_ref().unwrap().business_address,
        None
    );
}

#[test]
fn similar_facility_names_flags_the_real_milton_mix_up() {
    let facility_titles = vec!["Knapp's Self Stor of Milton Freewater - QMS Onboarding"];

    let similar =
        similar_facility_names_for("Milton Self Storage - New Elavon Account", &facility_titles);

    assert_eq!(
        similar,
        vec!["Knapp's Self Stor of Milton Freewater - QMS Onboarding".to_string()]
    );
}

#[test]
fn similar_facility_names_is_empty_for_an_unrelated_title() {
    let facility_titles = vec!["Highway 20 Self Storage - QMS Onboarding"];

    let similar = similar_facility_names_for(
        "Dubuqueland Mini Storage - New Elavon Account",
        &facility_titles,
    );

    assert!(similar.is_empty());
}

#[test]
fn similar_facility_names_deduplicates_a_title_repeated_across_ambiguous_candidate_rows() {
    // The same real facility appears once per `Correlation::Ambiguous`
    // candidate row -- must not show up twice in the same warning.
    let facility_titles = vec![
        "Knapp's Self Stor of Milton Freewater - QMS Onboarding",
        "Knapp's Self Stor of Milton Freewater - QMS Onboarding",
    ];

    let similar =
        similar_facility_names_for("Milton Self Storage - New Elavon Account", &facility_titles);

    assert_eq!(
        similar,
        vec!["Knapp's Self Stor of Milton Freewater - QMS Onboarding".to_string()]
    );
}
