//! Turning search hits into response rows: person-derived facilities, Merchant Account correlation and near-miss names.

use super::dto::{DuplicateCandidate, FacilityMatch, MatchedVia, PersonMatch};
use crate::clients::merchant_account_correlation::{
    addresses_fuzzy_match, shares_a_significant_word, Correlation,
};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};

/// One Merchant Account run's own display info, live-fetched once and
/// shared by whichever response row(s) it ends up on -- company name
/// (existing), plus the masked EIN/business address added 2026-09-23.
/// A failed fetch degrades to every field `None`/empty rather than
/// failing the whole search; this is a display enrichment, not
/// something the rest of the response depends on.
#[derive(Default, Clone)]
pub(super) struct MaDisplayInfo {
    pub(super) company_name: Option<String>,
    pub(super) ein_last_4: Option<String>,
    pub(super) business_address: Option<String>,
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
pub(super) fn derive_facilities_from_person_matches(
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
/// The values that identify one facility row before its Merchant Account
/// correlation is applied -- everything `facility_matches_for` threads
/// through unchanged.
pub(super) struct FacilityHit {
    pub(super) run_id: String,
    pub(super) run_name: String,
    pub(super) status: Option<String>,
    pub(super) matched_via: MatchedVia,
    pub(super) already_imported: bool,
    pub(super) last_activity_at: Option<DateTime<Utc>>,
}

/// The search-wide lookups every facility row reads from: each
/// correlated Merchant Account run's live display info, and when each
/// of those runs was last updated.
pub(super) struct DisplayLookups<'a> {
    pub(super) ma_display: &'a HashMap<String, MaDisplayInfo>,
    pub(super) merchant_account_updated_at: &'a HashMap<String, DateTime<Utc>>,
}

pub(super) fn facility_matches_for(
    hit: FacilityHit,
    correlation: Option<&Correlation>,
    lookups: &DisplayLookups,
) -> Vec<FacilityMatch> {
    let FacilityHit {
        run_id,
        run_name,
        status,
        matched_via,
        already_imported,
        last_activity_at,
    } = hit;
    let ma_display = lookups.ma_display;
    let merchant_account_updated_at = lookups.merchant_account_updated_at;
    let display_for = |ma_run_id: &str| ma_display.get(ma_run_id).cloned().unwrap_or_default();

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
            company_name: display_for(ma_run_id).company_name,
            run_id,
            run_name,
            status,
            already_imported,
            matched_via,
            last_activity_at,
            duplicate: None,
        }],
        Some(Correlation::Ambiguous(ma_run_ids)) => {
            // Whether every candidate that answered a business address
            // agrees with every other one that did -- `None` when fewer
            // than two have an address to compare at all. Consistent
            // addresses across candidates line up with a genuine
            // duplicate submission of the *same* application
            // (Carpentersville's own real case); addresses that
            // disagree line up with two different real businesses that
            // merely share a similar-sounding title (Knapp's Self Stor
            // of Milton Freewater's own real case). Decision support
            // only -- this never resolves the ambiguity on its own, it
            // just tells the human what to look at.
            let addresses: Vec<String> = ma_run_ids
                .iter()
                .filter_map(|id| display_for(id).business_address)
                .collect();
            let addresses_agree = if addresses.len() < 2 {
                None
            } else {
                Some(
                    addresses
                        .windows(2)
                        .all(|pair| addresses_fuzzy_match(&pair[0], &pair[1])),
                )
            };

            ma_run_ids
                .iter()
                .map(|ma_run_id| {
                    let display = display_for(ma_run_id);
                    FacilityMatch {
                        run_id: run_id.clone(),
                        run_name: run_name.clone(),
                        status: status.clone(),
                        already_imported,
                        matched_via: matched_via.clone(),
                        company_name: display.company_name,
                        last_activity_at,
                        duplicate: Some(DuplicateCandidate {
                            addresses_agree,
                            merchant_account_run_id: ma_run_id.clone(),
                            merchant_account_updated_at: *merchant_account_updated_at
                                .get(ma_run_id)
                                .expect("every candidate ma_run_id came from merchant_account_run_titles"),
                            ein_last_4: display.ein_last_4,
                            business_address: display.business_address,
                        }),
                    }
                })
                .collect()
        }
    }
}

/// Every facility title (from this same search's own results) that
/// shares a significant word with `run_name` -- see
/// `shares_a_significant_word`'s own doc comment for what that means
/// and why. Deduplicated (the same real facility can appear more than
/// once in `facility_titles`, once per `Correlation::Ambiguous`
/// candidate row) and sorted, so the response is stable rather than
/// whatever order a `HashSet` happens to iterate in.
pub(super) fn similar_facility_names_for(run_name: &str, facility_titles: &[&str]) -> Vec<String> {
    let mut similar: Vec<String> = facility_titles
        .iter()
        .filter(|title| shares_a_significant_word(run_name, title))
        .map(|title| title.to_string())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    similar.sort();
    similar
}
