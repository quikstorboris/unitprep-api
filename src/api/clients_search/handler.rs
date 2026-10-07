//! `GET /clients/search` -- the handler: it runs the two live Process
//! Street searches, then the local lookups (`lookup`), correlates
//! Merchant Accounts to the found facilities, enriches rows with live
//! display info (`display`) and assembles one response (`matching`).

use std::collections::HashMap;

use axum::extract::{Json, Query, State};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};

use super::display;
use super::dto::{
    FacilityMatch, MatchedVia, MerchantAccountMatch, SearchClientsQuery, SearchClientsResponse,
};
use super::lookup::{self, DbContext};
use super::matching::{
    facility_matches_for, similar_facility_names_for, DisplayLookups, FacilityHit,
};
use crate::api::{bad_request, internal_error, process_street_not_configured, AppState};
use crate::auth::AuthenticatedUser;
use crate::clients::merchant_account_correlation::{correlate_by_title, IntakeRunTitle};
use crate::clients::search::{search_by_facility_name, search_by_merchant_account_name};

pub async fn search_clients(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Query(query): Query<SearchClientsQuery>,
) -> Response {
    let q = query.q.trim();
    if q.is_empty() {
        return bad_request(
            "invalid_search_query",
            "q is required and must not be blank.".to_string(),
        );
    }

    let Some(client) = state.process_street.as_ref() else {
        tracing::warn!("client search attempted with Process Street not configured");
        return process_street_not_configured();
    };

    // The two live searches are independent of each other, so they run
    // together (this is the interactive search box: its latency is the
    // slower of the two, not their sum). See `super`'s own doc comment
    // (2026-09-14) -- a facility can have a real, live Merchant Account
    // run with no discoverable Intake run, so the second must be its own
    // live search, not something inferred from the first's results.
    let (facility_results, merchant_account_results) = tokio::join!(
        search_by_facility_name(client, q),
        search_by_merchant_account_name(client, q),
    );

    let facility_results = match facility_results {
        Ok(results) => results,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, query = %q, "Process Street facility-name search failed");
            return internal_error("Could not search Process Street");
        }
    };

    let merchant_account_results = match merchant_account_results {
        Ok(results) => results,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, query = %q, "Process Street merchant-account-name search failed");
            return internal_error("Could not search Process Street");
        }
    };

    let DbContext {
        person_matches,
        person_derived,
        already_imported,
        already_linked,
        intake_last_synced,
        merchant_account_titles,
        intake_universe,
    } = match lookup::load(
        &state.db,
        &user,
        q,
        &facility_results,
        &merchant_account_results,
    )
    .await
    {
        Ok(context) => context,
        Err(failure) => {
            tracing::error!(error = %failure.error, user_id = %user.user_id, query = %q, "{}", failure.log);
            return internal_error(failure.public);
        }
    };

    let intake_titles: Vec<IntakeRunTitle> = facility_results
        .iter()
        .map(|r| IntakeRunTitle {
            run_id: r.run_id.clone(),
            title_text: r.run_name.clone(),
        })
        .chain(
            person_derived
                .iter()
                .map(|(run_id, run_name, ..)| IntakeRunTitle {
                    run_id: run_id.clone(),
                    title_text: run_name.clone(),
                }),
        )
        .collect();
    let correlations =
        correlate_by_title(&intake_titles, &merchant_account_titles, &intake_universe);
    let merchant_account_updated_at: HashMap<String, DateTime<Utc>> = merchant_account_titles
        .iter()
        .map(|ma| (ma.run_id.clone(), ma.updated_at))
        .collect();

    let ma_display = display::correlated(client, &correlations, user.user_id).await;
    let lookups = DisplayLookups {
        ma_display: &ma_display,
        merchant_account_updated_at: &merchant_account_updated_at,
    };

    let mut facility_matches: Vec<FacilityMatch> = facility_results
        .into_iter()
        .flat_map(|r| {
            let hit = FacilityHit {
                already_imported: already_imported.contains(&r.run_id),
                run_id: r.run_id.clone(),
                run_name: r.run_name,
                status: Some(r.status),
                matched_via: MatchedVia::Name,
                last_activity_at: Some(r.updated_at),
            };
            facility_matches_for(hit, correlations.get(&r.run_id), &lookups)
        })
        .collect();

    facility_matches.extend(person_derived.into_iter().flat_map(
        |(run_id, run_name, full_name, role)| {
            let hit = FacilityHit {
                already_imported: already_imported.contains(&run_id),
                last_activity_at: intake_last_synced.get(&run_id).copied(),
                run_id: run_id.clone(),
                run_name,
                status: None,
                matched_via: MatchedVia::Person { full_name, role },
            };
            facility_matches_for(hit, correlations.get(&run_id), &lookups)
        },
    ));

    let mut standalone_display =
        display::standalone(client, &merchant_account_results, user.user_id).await;

    // Real facility titles from *this same search*, not the whole
    // database -- a near-miss warning is only useful against something
    // the user is actually looking at right now.
    let facility_titles: Vec<&str> = facility_matches
        .iter()
        .map(|m| m.run_name.as_str())
        .collect();

    let merchant_account_matches: Vec<MerchantAccountMatch> = merchant_account_results
        .into_iter()
        .map(|r| {
            let display = standalone_display.remove(&r.run_id).unwrap_or_default();
            let similar_facility_names = similar_facility_names_for(&r.run_name, &facility_titles);

            MerchantAccountMatch {
                already_linked: already_linked.contains(&r.run_id),
                run_id: r.run_id,
                run_name: r.run_name,
                status: r.status,
                updated_at: r.updated_at,
                ein_last_4: display.ein_last_4,
                business_address: display.business_address,
                similar_facility_names,
            }
        })
        .collect();

    tracing::info!(
        user_id = %user.user_id,
        query = %q,
        facility_match_count = facility_matches.len(),
        merchant_account_match_count = merchant_account_matches.len(),
        person_match_count = person_matches.len(),
        "user searched for a Process Street client"
    );

    Json(SearchClientsResponse {
        facility_matches,
        merchant_account_matches,
        person_matches,
    })
    .into_response()
}
