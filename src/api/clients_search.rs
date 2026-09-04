//! Search for a Process Street company/facility/person to import into
//! OO -- the entry point the "Add client from PS" flow (still Phase 3,
//! not built) will read from. Two genuinely different lookups running
//! side by side in one response:
//!
//! - **Facility matches**: a live call to PS's own server-side `name`
//!   filter over Intake runs only (`clients::search::search_by_facility_name`)
//!   -- cheap, no local index needed, always current. Each match also
//!   carries `already_imported`, a cheap local check against
//!   `clients.facilities.ps_intake_run_id` so a search result can be
//!   greyed out in the UI instead of silently inviting a duplicate
//!   import.
//! - **Person matches**: a local query against `clients.ps_person_index`
//!   (`clients::sync`'s delta-synced projection) -- PS has no
//!   server-side search over form-field values, so this is the only way
//!   to find a facility by an owner/DM/manager/signer/POC's name. Only
//!   as fresh as the last sync (`clients.ps_sync_state.last_synced_at`
//!   per run), not live. **Not query-text-only**: once a facility is
//!   matched (by title, or by a person hit -- see below), every OTHER
//!   person already indexed on that same Intake run is folded in too,
//!   so finding a facility surfaces its full known contact list, not
//!   just whichever one person's name happened to contain the query.
//!   (Boris, 2026-09-03: searching a facility by name, or by only one
//!   of its several owners, must not leave its other owners "behind".)
//!
//! **A company name (e.g. "Prairie Enterprises") doesn't literally
//! appear in a facility's own Intake run title** (real title:
//! "Highway 20 Self Storage - QMS Onboarding") -- so a query for the
//! company name alone can hit zero facility matches even though every
//! sister facility is right there under a shared owner/DM. Rather than
//! indexing a separate "company name" field (PS doesn't structurally
//! expose one that's reliably distinct from a facility's own name --
//! see the vault's sister-site writeup), this reuses the person index
//! that already exists for exactly this shape of problem: any Intake
//! run reachable via a person-name/email hit on the same query is
//! folded into `facility_matches` too, tagged `matched_via: person`
//! (never silently merged with a real title hit -- the UI must be able
//! to show *why* a result showed up). This is the same mechanism a
//! sister-facility suggestion ("you found Highway 20, here are its
//! likely sisters") would need, so it's built once, generically, keyed
//! off the search query itself rather than a specific already-selected
//! facility.
//!
//! Requires only authentication, not a particular permission -- same
//! reasoning as `client_ops_qms_tags::list_qms_tags`: this is read-only
//! discovery data (facility/person names), not a client operation in
//! its own right.

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Json, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clients::company_naming::resolve_company_name;
use crate::clients::merchant_account_correlation::{
    correlate_by_title, merchant_account_run_titles, Correlation, IntakeRunTitle,
};
use crate::clients::merchant_account_mapping::map_merchant_account_fields;
use crate::clients::search::search_by_facility_name;

#[derive(Debug, Deserialize)]
pub struct SearchClientsQuery {
    pub q: String,
}

/// Why a run showed up in `facility_matches` -- a literal PS title hit
/// carries a real `status`; a run pulled in only because a person on it
/// matched the query has no status available without an extra live PS
/// call per candidate, so it's `None` rather than guessed at.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MatchedVia {
    Name,
    Person { full_name: String, role: String },
}

/// Present only when this facility's Merchant Account correlation was
/// genuinely ambiguous (2+ distinct candidate runs, e.g. Carpentersville's
/// real duplicate submission) -- one `FacilityMatch` per candidate, all
/// sharing the same `run_id`/`run_name` (they're the same real
/// facility), each identified by which Merchant Account run it came
/// from. The frontend brackets rows sharing a `run_id` and shows this
/// as "Potential Duplicates" rather than silently picking one.
#[derive(Debug, Serialize)]
pub struct DuplicateCandidate {
    pub merchant_account_run_id: String,
    /// PS's own `audit.updatedDate` for *this* Merchant Account run --
    /// deliberately separate from `FacilityMatch::last_activity_at`
    /// (the shared facility's own Intake activity, identical across
    /// every duplicate row) since this is the value that actually
    /// differs between candidates and helps a user tell which one is
    /// the stale duplicate.
    pub merchant_account_updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct FacilityMatch {
    pub run_id: String,
    pub run_name: String,
    pub status: Option<String>,
    /// Whether `clients.facilities` already has a row for this Intake
    /// run -- lets the UI grey this match out rather than inviting a
    /// duplicate "Add".
    pub already_imported: bool,
    pub matched_via: MatchedVia,
    /// The company this facility belongs to, per the same
    /// Merchant-Account-Legal-Name-first rule `resolve_company_name`
    /// already applies at import time -- `None` when no Merchant
    /// Account run could be confidently correlated to this facility's
    /// own Intake run (not every client uses Elavon).
    pub company_name: Option<String>,
    /// PS's own `audit.updatedDate` for this facility's own Intake run
    /// -- live for a literal title match, last-synced for a
    /// person-derived one (see this module's own `search_clients`
    /// body). Helps a user judge how current/relevant a result is.
    pub last_activity_at: Option<DateTime<Utc>>,
    pub duplicate: Option<DuplicateCandidate>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PersonMatch {
    /// `intake` | `merchant_account` | `contract_order`.
    pub workflow: String,
    pub ps_run_id: String,
    pub run_name: String,
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub role: String,
}

#[derive(Debug, Serialize)]
pub struct SearchClientsResponse {
    pub facility_matches: Vec<FacilityMatch>,
    pub person_matches: Vec<PersonMatch>,
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "invalid_search_query",
            message: message.to_string(),
        }),
    )
        .into_response()
}

fn process_street_not_configured() -> Response {
    tracing::warn!("client search attempted with Process Street not configured");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiErrorBody {
            error: "process_street_not_configured",
            message: "Process Street integration is not configured on this server.".to_string(),
        }),
    )
        .into_response()
}

/// A run pulled into `facility_matches` only because a person on it
/// matched the query, not because its own title did -- `(run_id,
/// run_name, matched person's full_name, matched person's role)`.
///
/// Only Intake-workflow rows are eligible (a Merchant Account/Contract
/// Order run id isn't a facility identity -- see `clients::search`'s
/// own Intake-only scoping), runs already present via a literal title
/// hit are excluded (never double-list the same run), and only the
/// first person match per run is kept as the shown reason -- whichever
/// comes first in `person_matches`' own order (by `full_name`).
fn derive_facilities_from_person_matches(
    person_matches: &[PersonMatch],
    literal_run_ids: &std::collections::HashSet<&str>,
) -> Vec<(String, String, String, String)> {
    let mut derived = Vec::new();
    let mut seen_run_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();

    for pm in person_matches {
        if pm.workflow != "intake" {
            continue;
        }
        if literal_run_ids.contains(pm.ps_run_id.as_str()) {
            continue;
        }
        if !seen_run_ids.insert(pm.ps_run_id.as_str()) {
            continue;
        }
        derived.push((
            pm.ps_run_id.clone(),
            pm.run_name.clone(),
            pm.full_name.clone(),
            pm.role.clone(),
        ));
    }

    derived
}

/// Expands one facility (a literal title hit or a person-derived one)
/// into the `FacilityMatch` row(s) it becomes -- one row normally, or
/// one row *per candidate* when its Merchant Account correlation was
/// ambiguous (see `DuplicateCandidate`'s own doc comment). All the
/// per-row values that don't vary by candidate (`run_id`, `run_name`,
/// `status`, `already_imported`, `matched_via`, `last_activity_at`)
/// are threaded through unchanged; only `company_name` and `duplicate`
/// differ per row.
#[allow(clippy::too_many_arguments)]
fn facility_matches_for(
    run_id: String,
    run_name: String,
    status: Option<String>,
    matched_via: MatchedVia,
    already_imported: bool,
    last_activity_at: Option<DateTime<Utc>>,
    correlation: Option<&Correlation>,
    company_names: &HashMap<String, Option<String>>,
    merchant_account_updated_at: &HashMap<String, DateTime<Utc>>,
) -> Vec<FacilityMatch> {
    let company_name_for = |ma_run_id: &str| company_names.get(ma_run_id).cloned().flatten();

    match correlation {
        None => vec![FacilityMatch {
            run_id,
            run_name,
            status,
            already_imported,
            matched_via,
            company_name: None,
            last_activity_at,
            duplicate: None,
        }],
        Some(Correlation::Unambiguous(ma_run_id)) => vec![FacilityMatch {
            company_name: company_name_for(ma_run_id),
            run_id,
            run_name,
            status,
            already_imported,
            matched_via,
            last_activity_at,
            duplicate: None,
        }],
        Some(Correlation::Ambiguous(ma_run_ids)) => ma_run_ids
            .iter()
            .map(|ma_run_id| FacilityMatch {
                run_id: run_id.clone(),
                run_name: run_name.clone(),
                status: status.clone(),
                already_imported,
                matched_via: matched_via.clone(),
                company_name: company_name_for(ma_run_id),
                last_activity_at,
                duplicate: Some(DuplicateCandidate {
                    merchant_account_run_id: ma_run_id.clone(),
                    merchant_account_updated_at: *merchant_account_updated_at
                        .get(ma_run_id)
                        .expect("every candidate ma_run_id came from merchant_account_run_titles"),
                }),
            })
            .collect(),
    }
}

pub async fn search_clients(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Query(query): Query<SearchClientsQuery>,
) -> Response {
    let q = query.q.trim();
    if q.is_empty() {
        return bad_request("q is required and must not be blank.");
    }

    let Some(client) = state.process_street.as_ref() else {
        return process_street_not_configured();
    };

    let facility_results = match search_by_facility_name(client, q).await {
        Ok(results) => results,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, query = %q, "Process Street facility-name search failed");
            return internal_error("Could not search Process Street");
        }
    };

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for search");
            return internal_error("Could not search Process Street");
        }
    };

    // A leading-wildcard ILIKE, not an indexed lookup -- see the
    // ps_person_index migration's own comment on why a full-text/trigram
    // index isn't worth it yet at this data's real scale. Capped at 50:
    // this is a picker, not a report, and an unbounded scan risk grows
    // with a substring query against every indexed run.
    let person_matches: Result<Vec<PersonMatch>, sqlx::Error> = sqlx::query_as(
        "SELECT workflow, ps_run_id, run_name, full_name, email, phone, role
           FROM clients.ps_person_index
          WHERE full_name ILIKE '%' || $1 || '%'
             OR email ILIKE '%' || $1 || '%'
          ORDER BY full_name
          LIMIT 50",
    )
    .bind(q)
    .fetch_all(&mut *tx)
    .await;

    let person_matches = match person_matches {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, query = %q, "person-index search query failed");
            return internal_error("Could not search for a person by name");
        }
    };

    // Runs pulled in only via a person hit -- e.g. searching a company
    // name like "Prairie Enterprises" won't literally match any
    // facility's own Intake title, but its owner/DM will show up in
    // person_matches on every one of that company's facilities.
    let literal_run_ids: std::collections::HashSet<&str> =
        facility_results.iter().map(|r| r.run_id.as_str()).collect();
    let person_derived = derive_facilities_from_person_matches(&person_matches, &literal_run_ids);

    let mut candidate_run_ids: Vec<String> = facility_results.iter().map(|r| r.run_id.clone()).collect();
    candidate_run_ids.extend(person_derived.iter().map(|(run_id, ..)| run_id.clone()));

    // Every person already indexed under a facility that matched (by
    // title or via a person hit) -- not just the ones whose own name/
    // email happened to contain the query text. Without this, finding
    // a facility by its own name (or by ONE of its several owners) only
    // ever surfaces that one person, and every other real contact on
    // the same facility silently "falls behind" -- Boris, 2026-09-03,
    // after Sand-Sto's second owner never showed up next to the first.
    // `workflow = 'intake'` only: that's the same scoping
    // `derive_facilities_from_person_matches` already applies (a
    // Merchant Account/Contract Order run id isn't a facility identity
    // of its own), and it's the workflow this index actually keys
    // facility ownership on.
    let facility_person_matches: Result<Vec<PersonMatch>, sqlx::Error> = sqlx::query_as(
        "SELECT workflow, ps_run_id, run_name, full_name, email, phone, role
           FROM clients.ps_person_index
          WHERE workflow = 'intake' AND ps_run_id = ANY($1)
          ORDER BY full_name",
    )
    .bind(&candidate_run_ids)
    .fetch_all(&mut *tx)
    .await;

    let mut person_matches = person_matches;
    match facility_person_matches {
        Ok(rows) => {
            let mut seen: HashSet<(String, String, String)> = person_matches
                .iter()
                .map(|p| (p.ps_run_id.clone(), p.full_name.clone(), p.role.clone()))
                .collect();
            for row in rows {
                let key = (row.ps_run_id.clone(), row.full_name.clone(), row.role.clone());
                if seen.insert(key) {
                    person_matches.push(row);
                }
            }
            person_matches.sort_by(|a, b| a.full_name.cmp(&b.full_name));
        }
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility person-index fetch failed");
            return internal_error("Could not search for a person by name");
        }
    }

    let already_imported_result: Result<Vec<(String,)>, sqlx::Error> = sqlx::query_as(
        "SELECT ps_intake_run_id FROM clients.facilities WHERE ps_intake_run_id = ANY($1)",
    )
    .bind(&candidate_run_ids)
    .fetch_all(&mut *tx)
    .await;

    let already_imported: std::collections::HashSet<String> = match already_imported_result {
        Ok(rows) => rows.into_iter().map(|(id,)| id).collect(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "already-imported facility check failed");
            return internal_error("Could not search Process Street");
        }
    };

    // Only for person-derived rows -- a literal title match already has
    // its own live `updated_at` straight from the search call itself
    // (`SearchResult::updated_at`), which is fresher than this.
    let intake_last_synced_result: Result<Vec<(String, DateTime<Utc>)>, sqlx::Error> = sqlx::query_as(
        "SELECT ps_run_id, ps_updated_at FROM clients.ps_sync_state
          WHERE workflow = 'intake' AND ps_run_id = ANY($1)",
    )
    .bind(&candidate_run_ids)
    .fetch_all(&mut *tx)
    .await;

    let intake_last_synced: HashMap<String, DateTime<Utc>> = match intake_last_synced_result {
        Ok(rows) => rows.into_iter().collect(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "intake last-activity fetch failed");
            return internal_error("Could not search Process Street");
        }
    };

    let merchant_account_titles = match merchant_account_run_titles(&mut tx).await {
        Ok(titles) => titles,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "merchant-account title fetch failed");
            return internal_error("Could not search Process Street");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit person-name search transaction");
        return internal_error("Could not search for a person by name");
    }

    let intake_titles: Vec<IntakeRunTitle> = facility_results
        .iter()
        .map(|r| IntakeRunTitle { run_id: r.run_id.clone(), title_text: r.run_name.clone() })
        .chain(person_derived.iter().map(|(run_id, run_name, ..)| IntakeRunTitle {
            run_id: run_id.clone(),
            title_text: run_name.clone(),
        }))
        .collect();
    let correlations = correlate_by_title(&intake_titles, &merchant_account_titles);
    let merchant_account_updated_at: HashMap<String, DateTime<Utc>> = merchant_account_titles
        .iter()
        .map(|ma| (ma.run_id.clone(), ma.updated_at))
        .collect();

    // One live PS call per *distinct* correlated Merchant Account run,
    // concurrently, not one after another -- typically a handful at
    // most for a single company's worth of search results. Every
    // ambiguous candidate gets its own fetch too, not just unambiguous
    // ones, since a "Potential Duplicates" row still needs its own
    // suggested company name. A failed fetch degrades to "no company
    // name" for that one candidate rather than failing the whole
    // search; this is a display enrichment, not something the rest of
    // the response depends on.
    let distinct_ma_run_ids: HashSet<&str> = correlations
        .values()
        .flat_map(|c| match c {
            Correlation::Unambiguous(id) => std::slice::from_ref(id),
            Correlation::Ambiguous(ids) => ids.as_slice(),
        })
        .map(String::as_str)
        .collect();
    let ma_fetches = distinct_ma_run_ids.iter().map(|ma_run_id| async move {
        (*ma_run_id, client.get_run_form_fields(ma_run_id).await)
    });
    let mut company_names: HashMap<String, Option<String>> = HashMap::new();
    for (ma_run_id, result) in futures::future::join_all(ma_fetches).await {
        let company_name = match result {
            Ok(fields) => resolve_company_name(None, Some(&map_merchant_account_fields(&fields))),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    user_id = %user.user_id,
                    ma_run_id = %ma_run_id,
                    "failed to fetch correlated Merchant Account run's fields for company-name display"
                );
                None
            }
        };
        company_names.insert(ma_run_id.to_string(), company_name);
    }

    let mut facility_matches: Vec<FacilityMatch> = facility_results
        .into_iter()
        .flat_map(|r| {
            facility_matches_for(
                r.run_id.clone(),
                r.run_name,
                Some(r.status),
                MatchedVia::Name,
                already_imported.contains(&r.run_id),
                Some(r.updated_at),
                correlations.get(&r.run_id),
                &company_names,
                &merchant_account_updated_at,
            )
        })
        .collect();

    facility_matches.extend(person_derived.into_iter().flat_map(|(run_id, run_name, full_name, role)| {
        let last_activity_at = intake_last_synced.get(&run_id).copied();
        facility_matches_for(
            run_id.clone(),
            run_name,
            None,
            MatchedVia::Person { full_name, role },
            already_imported.contains(&run_id),
            last_activity_at,
            correlations.get(&run_id),
            &company_names,
            &merchant_account_updated_at,
        )
    }));

    tracing::info!(
        user_id = %user.user_id,
        query = %q,
        facility_match_count = facility_matches.len(),
        person_match_count = person_matches.len(),
        "user searched for a Process Street client"
    );

    Json(SearchClientsResponse {
        facility_matches,
        person_matches,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn blank_query_is_rejected_without_touching_anything() {
        let response = search_clients(
            State(empty_state()),
            test_user(),
            Query(SearchClientsQuery { q: "   ".to_string() }),
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
            Query(SearchClientsQuery { q: "highway".to_string() }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    fn person_match(workflow: &str, run_id: &str, run_name: &str, full_name: &str, role: &str) -> PersonMatch {
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
        assert_eq!(derived[0].2, "Judy Armstrong", "first match in order wins as the shown reason");
    }

    fn no_correlation_context() -> (
        HashMap<String, Option<String>>,
        HashMap<String, chrono::DateTime<chrono::Utc>>,
    ) {
        (HashMap::new(), HashMap::new())
    }

    #[test]
    fn no_correlation_produces_one_row_with_no_company_name_or_duplicate() {
        let (company_names, ma_updated_at) = no_correlation_context();

        let matches = facility_matches_for(
            "run-solo".to_string(),
            "Solo Storage - QMS Onboarding".to_string(),
            Some("Active".to_string()),
            MatchedVia::Name,
            false,
            None,
            None,
            &company_names,
            &ma_updated_at,
        );

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].company_name, None);
        assert!(matches[0].duplicate.is_none());
    }

    #[test]
    fn an_unambiguous_correlation_produces_one_row_with_a_resolved_company_name() {
        let mut company_names = HashMap::new();
        company_names.insert("ma-highway-20".to_string(), Some("Prairie Enterprises LLC".to_string()));
        let ma_updated_at = HashMap::new();
        let correlation = Correlation::Unambiguous("ma-highway-20".to_string());

        let matches = facility_matches_for(
            "run-highway-20".to_string(),
            "Highway 20 Self Storage - QMS Onboarding".to_string(),
            Some("Active".to_string()),
            MatchedVia::Name,
            false,
            None,
            Some(&correlation),
            &company_names,
            &ma_updated_at,
        );

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].company_name.as_deref(), Some("Prairie Enterprises LLC"));
        assert!(matches[0].duplicate.is_none());
    }

    #[test]
    fn an_ambiguous_correlation_produces_one_row_per_candidate_sharing_the_same_facility_identity() {
        // The real Carpentersville case: two distinct, identically
        // titled Merchant Account runs, each resolving to its own
        // (possibly differing) suggested company name.
        let mut company_names = HashMap::new();
        company_names.insert("ma-carpentersville-1".to_string(), Some("Prairie Enterprises LLC".to_string()));
        company_names.insert("ma-carpentersville-2".to_string(), Some("Carpentersville Self Storage".to_string()));
        let mut ma_updated_at = HashMap::new();
        let older = chrono::DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z").unwrap().to_utc();
        let newer = chrono::DateTime::parse_from_rfc3339("2026-08-30T00:00:00Z").unwrap().to_utc();
        ma_updated_at.insert("ma-carpentersville-1".to_string(), newer);
        ma_updated_at.insert("ma-carpentersville-2".to_string(), older);
        let correlation = Correlation::Ambiguous(vec![
            "ma-carpentersville-1".to_string(),
            "ma-carpentersville-2".to_string(),
        ]);

        let matches = facility_matches_for(
            "run-carpentersville".to_string(),
            "Carpentersville Self Storage - QMS Onboarding".to_string(),
            Some("Active".to_string()),
            MatchedVia::Name,
            false,
            None,
            Some(&correlation),
            &company_names,
            &ma_updated_at,
        );

        assert_eq!(matches.len(), 2);
        // Every row shares the same real facility's identity -- the
        // frontend brackets on this.
        assert!(matches.iter().all(|m| m.run_id == "run-carpentersville"));

        let candidate_1 = matches
            .iter()
            .find(|m| m.duplicate.as_ref().unwrap().merchant_account_run_id == "ma-carpentersville-1")
            .expect("candidate 1 present");
        assert_eq!(candidate_1.company_name.as_deref(), Some("Prairie Enterprises LLC"));
        assert_eq!(candidate_1.duplicate.as_ref().unwrap().merchant_account_updated_at, newer);

        let candidate_2 = matches
            .iter()
            .find(|m| m.duplicate.as_ref().unwrap().merchant_account_run_id == "ma-carpentersville-2")
            .expect("candidate 2 present");
        assert_eq!(candidate_2.company_name.as_deref(), Some("Carpentersville Self Storage"));
        assert_eq!(candidate_2.duplicate.as_ref().unwrap().merchant_account_updated_at, older);
    }
}
