//! The local database half of a client search, run in one RLS transaction
//! after the live Process Street searches: people matching the query,
//! which of the found runs are already imported/linked, when intake runs
//! were last synced, and the title universes the Merchant Account
//! correlation needs.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};

use super::dto::PersonMatch;
use super::matching::derive_facilities_from_person_matches;
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clients::merchant_account_correlation::{
    all_intake_run_titles, merchant_account_run_titles, IntakeRunTitle, MerchantAccountRunInfo,
};
use crate::clients::search::SearchResult;

/// A lookup failed: what to log, what to tell the caller, and the error.
/// The handler logs once, so each step only has to say which step it was.
pub(super) struct LookupFailure {
    pub(super) log: &'static str,
    pub(super) public: &'static str,
    pub(super) error: sqlx::Error,
}

const SEARCH_FAILED: &str = "Could not search Process Street";
const PERSON_SEARCH_FAILED: &str = "Could not search for a person by name";

fn fail(log: &'static str, public: &'static str) -> impl FnOnce(sqlx::Error) -> LookupFailure {
    move |error| LookupFailure { log, public, error }
}

/// Everything the handler reads from the database for one search.
pub(super) struct DbContext {
    /// People matching the query, plus every other person already indexed
    /// under a facility that matched (see `load`'s own comment).
    pub(super) person_matches: Vec<PersonMatch>,
    /// Runs pulled in only via a person hit: `(run_id, run_name, matched
    /// person's full_name, matched person's role)`.
    pub(super) person_derived: Vec<(String, String, String, String)>,
    pub(super) already_imported: HashSet<String>,
    pub(super) already_linked: HashSet<String>,
    pub(super) intake_last_synced: HashMap<String, DateTime<Utc>>,
    pub(super) merchant_account_titles: Vec<MerchantAccountRunInfo>,
    pub(super) intake_universe: Vec<IntakeRunTitle>,
}

pub(super) async fn load(
    db: &sqlx::PgPool,
    user: &AuthenticatedUser,
    q: &str,
    facility_results: &[SearchResult],
    merchant_account_results: &[SearchResult],
) -> Result<DbContext, LookupFailure> {
    let mut tx = begin_rls_transaction(db, user.user_id, &user.role_keys)
        .await
        .map_err(fail("failed to open transaction for search", SEARCH_FAILED))?;

    // A leading-wildcard ILIKE, not an indexed lookup -- see the
    // ps_person_index migration's own comment on why a full-text/trigram
    // index isn't worth it yet at this data's real scale. Capped at 50:
    // this is a picker, not a report, and an unbounded scan risk grows
    // with a substring query against every indexed run.
    let person_matches: Vec<PersonMatch> = sqlx::query_as(
        "SELECT workflow, ps_run_id, run_name, full_name, email, phone, role
           FROM clients.ps_person_index
          WHERE full_name ILIKE '%' || $1 || '%'
             OR email ILIKE '%' || $1 || '%'
          ORDER BY full_name
          LIMIT 50",
    )
    .bind(q)
    .fetch_all(&mut *tx)
    .await
    .map_err(fail(
        "person-index search query failed",
        PERSON_SEARCH_FAILED,
    ))?;

    // Runs pulled in only via a person hit -- e.g. searching a company
    // name like "Prairie Enterprises" won't literally match any
    // facility's own Intake title, but its owner/DM will show up in
    // person_matches on every one of that company's facilities.
    let literal_run_ids: HashSet<&str> =
        facility_results.iter().map(|r| r.run_id.as_str()).collect();
    let person_derived = derive_facilities_from_person_matches(&person_matches, &literal_run_ids);

    let mut candidate_run_ids: Vec<String> =
        facility_results.iter().map(|r| r.run_id.clone()).collect();
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
    let facility_person_matches: Vec<PersonMatch> = sqlx::query_as(
        "SELECT workflow, ps_run_id, run_name, full_name, email, phone, role
           FROM clients.ps_person_index
          WHERE workflow = 'intake' AND ps_run_id = ANY($1)
          ORDER BY full_name",
    )
    .bind(&candidate_run_ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(fail(
        "facility person-index fetch failed",
        PERSON_SEARCH_FAILED,
    ))?;

    let mut person_matches = person_matches;
    let mut seen: HashSet<(String, String, String)> = person_matches
        .iter()
        .map(|p| (p.ps_run_id.clone(), p.full_name.clone(), p.role.clone()))
        .collect();
    for row in facility_person_matches {
        let key = (
            row.ps_run_id.clone(),
            row.full_name.clone(),
            row.role.clone(),
        );
        if seen.insert(key) {
            person_matches.push(row);
        }
    }
    person_matches.sort_by(|a, b| a.full_name.cmp(&b.full_name));

    let already_imported: HashSet<String> = sqlx::query_as::<_, (String,)>(
        "SELECT ps_intake_run_id FROM clients.facilities WHERE ps_intake_run_id = ANY($1)",
    )
    .bind(&candidate_run_ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(fail(
        "already-imported facility check failed",
        SEARCH_FAILED,
    ))?
    .into_iter()
    .map(|(id,)| id)
    .collect();

    let merchant_account_run_ids: Vec<String> = merchant_account_results
        .iter()
        .map(|r| r.run_id.clone())
        .collect();
    let already_linked: HashSet<String> = sqlx::query_as::<_, (String,)>(
        "SELECT ps_new_merchant_run_id FROM clients.facility_merchant_accounts
          WHERE ps_new_merchant_run_id = ANY($1)",
    )
    .bind(&merchant_account_run_ids)
    .fetch_all(&mut *tx)
    .await
    .map_err(fail(
        "already-linked merchant account check failed",
        SEARCH_FAILED,
    ))?
    .into_iter()
    .map(|(id,)| id)
    .collect();

    // Only for person-derived rows -- a literal title match already has
    // its own live `updated_at` straight from the search call itself
    // (`SearchResult::updated_at`), which is fresher than this.
    let intake_last_synced: HashMap<String, DateTime<Utc>> =
        sqlx::query_as::<_, (String, DateTime<Utc>)>(
            "SELECT ps_run_id, ps_updated_at FROM clients.ps_sync_state
          WHERE workflow = 'intake' AND ps_run_id = ANY($1)",
        )
        .bind(&candidate_run_ids)
        .fetch_all(&mut *tx)
        .await
        .map_err(fail("intake last-activity fetch failed", SEARCH_FAILED))?
        .into_iter()
        .collect();

    let merchant_account_titles = merchant_account_run_titles(&mut tx)
        .await
        .map_err(fail("merchant-account title fetch failed", SEARCH_FAILED))?;

    let intake_universe = all_intake_run_titles(&mut tx)
        .await
        .map_err(fail("intake title universe fetch failed", SEARCH_FAILED))?;

    tx.commit().await.map_err(fail(
        "failed to commit person-name search transaction",
        PERSON_SEARCH_FAILED,
    ))?;

    Ok(DbContext {
        person_matches,
        person_derived,
        already_imported,
        already_linked,
        intake_last_synced,
        merchant_account_titles,
        intake_universe,
    })
}
