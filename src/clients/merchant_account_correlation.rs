//! Correlates an Intake run to its own New Merchant Account run, purely
//! off already-locally-indexed data (no live PS call, no dependency on
//! `clients.facility_merchant_accounts` -- this runs pre-import). PS has
//! no direct link between an Intake run and a Merchant Account run for
//! the same real facility (see the vault's Phase 3 notes on why that's
//! "the hard part").
//!
//! Shared by two callers that each need it for a different reason:
//! `api::clients_search` (a "Company" column on search results, plus
//! surfacing genuinely ambiguous cases as "Potential Duplicates" rows
//! rather than silently dropping them) and `api::clients_preview` (a
//! suggested Legal Name on the confirmation screen, via
//! `company_naming::resolve_company_name` -- ambiguous cases are
//! simply skipped there, since that screen isn't where duplicates get
//! resolved) -- moved here rather than duplicated or imported
//! cross-handler.
//!
//! **2026-09-02: replaced shared-owner-email correlation entirely**,
//! confirmed broken against real Prairie Enterprises data, not a
//! hypothetical: Kyle Lindley (the real owner) uses
//! `k.lindley@prairie-enterprises.com` on his Intake runs but
//! `kyle.lindley@outlook.com` on every Merchant Account application --
//! a different email for the same person, so a shared-email join found
//! zero matches. Shared-*name* isn't a fix either: Kyle Lindley is
//! listed as owner on every one of Prairie's sister facilities'
//! Merchant Account runs, not just his own, so name-matching Highway
//! 20's Intake run against "any Merchant Account run mentioning Kyle
//! Lindley" is ambiguous across all of them -- a real multi-facility
//! owner appearing everywhere isn't a data-quality accident, it's the
//! normal case a shared-person signal can't discriminate.
//!
//! **What actually discriminates one facility's Merchant Account run
//! from its sisters': PS's own title convention.** Real observed
//! titles: Intake "Highway 20 Self Storage - QMS Onboarding" vs.
//! Merchant Account "Prairie Enterprises (Highway 20)" -- the
//! parenthetical is the facility nickname, and it's specific to that
//! one facility ("Carpentersville", "Pyott Road", ...), unlike the
//! owner's name or email. This is also confirmed elsewhere in the
//! vault's own Phase 2 search notes ("the same facility genuinely has
//! a different run title per workflow").
//!
//! **2026-09-02: real genuine ambiguity found too** -- Carpentersville
//! has two distinct, identically-titled Merchant Account runs in the
//! real data (apparently a real duplicate submission, still
//! unresolved as of this writing). `correlate_by_title` surfaces this
//! as `Correlation::Ambiguous` rather than just dropping it, so
//! `clients_search` can show both candidates as "Potential Duplicate"
//! rows instead of silently leaving Company blank with no explanation.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

/// One locally-indexed Merchant Account run's own identity.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MerchantAccountRunInfo {
    pub run_id: String,
    pub run_name: String,
    /// PS's own `audit.updatedDate` as of the last sync -- not live,
    /// but this is exactly the value that lets a user tell which of
    /// two duplicate runs is the stale one without leaving this app.
    pub updated_at: DateTime<Utc>,
}

/// Fetches every locally-indexed Merchant Account run's own identity --
/// from `ps_sync_state`, not `ps_person_index`, since a run with zero
/// indexed people (an edge case `ps_person_index` alone wouldn't cover)
/// still has a title and an `updated_at`.
pub async fn merchant_account_run_titles(
    tx: &mut Transaction<'_, Postgres>,
) -> Result<Vec<MerchantAccountRunInfo>, sqlx::Error> {
    sqlx::query_as(
        "SELECT ps_run_id AS run_id, run_name, ps_updated_at AS updated_at
           FROM clients.ps_sync_state
          WHERE workflow = 'merchant_account'",
    )
    .fetch_all(&mut **tx)
    .await
}

/// One Intake run's own identifying text to search for inside a
/// Merchant Account run's parenthetical nickname -- callers pass
/// whichever they already have cheaply: `clients_search` uses the raw
/// PS run title (no extra fetch), `clients_preview` uses the mapped
/// `facility.name` field (already fetched for other reasons). Either
/// works the same way here: both contain the facility's own name.
pub struct IntakeRunTitle {
    pub run_id: String,
    pub title_text: String,
}

/// What `correlate_by_title` found for one Intake run.
#[derive(Debug, Clone, PartialEq)]
pub enum Correlation {
    /// Exactly one Merchant Account run matched -- safe to treat as
    /// this facility's own.
    Unambiguous(String),
    /// Two or more distinct Merchant Account runs matched (a real
    /// duplicate submission, confirmed against Prairie's own data --
    /// see this module's own doc comment) -- never picked automatically.
    /// Ordered by `updated_at` descending (most-recently-active first)
    /// by the caller building the response, not here; this just
    /// carries every candidate.
    Ambiguous(Vec<String>),
}

/// Extracts the text inside a run title's first `(...)`, e.g.
/// `"Prairie Enterprises (Highway 20)"` -> `Some("Highway 20")`.
/// `None` for a title with no parenthetical at all (a real, common
/// case -- not every company's Merchant Account title follows this
/// pattern, e.g. a genuine sole-prop title might just be a name).
fn parenthetical(run_name: &str) -> Option<&str> {
    let start = run_name.find('(')?;
    let rel_end = run_name[start..].find(')')?;
    let inner = run_name[start + 1..start + rel_end].trim();
    if inner.is_empty() {
        None
    } else {
        Some(inner)
    }
}

/// Correlates each Intake run in `intake_runs` against every Merchant
/// Account run in `merchant_account_runs`, by checking whether a
/// Merchant Account run's own parenthetical nickname appears
/// (case-insensitive substring) inside the Intake run's own title
/// text. An Intake run matching zero Merchant Account runs is simply
/// absent from the result -- matching two or more is surfaced as
/// `Correlation::Ambiguous`, not silently dropped (see this module's
/// own doc comment for why that distinction matters for real data).
pub fn correlate_by_title(
    intake_runs: &[IntakeRunTitle],
    merchant_account_runs: &[MerchantAccountRunInfo],
) -> HashMap<String, Correlation> {
    let mut candidates: HashMap<&str, HashSet<&str>> = HashMap::new();

    for ma in merchant_account_runs {
        let Some(keyword) = parenthetical(&ma.run_name) else {
            continue;
        };
        let keyword_lower = keyword.to_lowercase();

        for intake in intake_runs {
            if intake.title_text.to_lowercase().contains(&keyword_lower) {
                candidates.entry(&intake.run_id).or_default().insert(ma.run_id.as_str());
            }
        }
    }

    candidates
        .into_iter()
        .map(|(run_id, ma_ids)| {
            let mut ids: Vec<String> = ma_ids.into_iter().map(str::to_string).collect();
            ids.sort();
            let correlation = if ids.len() == 1 {
                Correlation::Unambiguous(ids.into_iter().next().expect("len == 1"))
            } else {
                Correlation::Ambiguous(ids)
            };
            (run_id.to_string(), correlation)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intake(run_id: &str, title_text: &str) -> IntakeRunTitle {
        IntakeRunTitle {
            run_id: run_id.to_string(),
            title_text: title_text.to_string(),
        }
    }

    fn ma(run_id: &str, run_name: &str) -> MerchantAccountRunInfo {
        MerchantAccountRunInfo {
            run_id: run_id.to_string(),
            run_name: run_name.to_string(),
            updated_at: DateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn parenthetical_extracts_the_facility_nickname() {
        assert_eq!(parenthetical("Prairie Enterprises (Highway 20)"), Some("Highway 20"));
    }

    #[test]
    fn parenthetical_is_none_without_a_real_facility_name_inside() {
        assert_eq!(parenthetical("Prairie Enterprises LLC"), None);
        assert_eq!(parenthetical("Empty Parens ()"), None);
    }

    // Real Prairie Enterprises data, captured 2026-09-02 -- proves the
    // fix against the exact case that was broken, not a synthetic one.
    #[test]
    fn real_prairie_data_correlates_each_facility_to_its_own_merchant_account_run() {
        let intake_runs = vec![
            intake("intake-highway-20", "Highway 20 Self Storage - QMS Onboarding"),
            intake("intake-carpentersville", "Carpentersville Self Storage - QMS Onboarding"),
            intake("intake-pyott-road", "Pyott Road Self Storage - QMS Onboarding"),
        ];
        let merchant_account_runs = vec![
            ma("ma-highway-20", "Prairie Enterprises (Highway 20)"),
            ma("ma-carpentersville-1", "Prairie Enterprises (Carpentersville)"),
            ma("ma-carpentersville-2", "Prairie Enterprises (Carpentersville)"),
            ma("ma-pyott-road", "Prairie Enterprises (Pyott Road)"),
        ];

        let correlated = correlate_by_title(&intake_runs, &merchant_account_runs);

        assert_eq!(
            correlated.get("intake-highway-20"),
            Some(&Correlation::Unambiguous("ma-highway-20".to_string()))
        );
        assert_eq!(
            correlated.get("intake-pyott-road"),
            Some(&Correlation::Unambiguous("ma-pyott-road".to_string()))
        );
        // Carpentersville has two distinct, identically-titled Merchant
        // Account runs in the real data (a real duplicate submission) --
        // genuinely ambiguous, surfaced as both candidates rather than
        // guessed at or dropped.
        assert_eq!(
            correlated.get("intake-carpentersville"),
            Some(&Correlation::Ambiguous(vec![
                "ma-carpentersville-1".to_string(),
                "ma-carpentersville-2".to_string()
            ]))
        );
    }

    #[test]
    fn a_shared_owner_across_every_sister_facility_no_longer_causes_false_ambiguity() {
        // This is the exact failure mode the old email/name-based
        // correlation had: Kyle Lindley is listed as owner on every
        // sister facility's Merchant Account run, not just his own.
        // Title correlation doesn't look at people at all, so it isn't
        // affected by that at all.
        let intake_runs = vec![intake("intake-highway-20", "Highway 20 Self Storage - QMS Onboarding")];
        let merchant_account_runs = vec![
            ma("ma-highway-20", "Prairie Enterprises (Highway 20)"),
            ma("ma-carpentersville", "Prairie Enterprises (Carpentersville)"),
            ma("ma-pyott-road", "Prairie Enterprises (Pyott Road)"),
        ];

        let correlated = correlate_by_title(&intake_runs, &merchant_account_runs);

        assert_eq!(
            correlated.get("intake-highway-20"),
            Some(&Correlation::Unambiguous("ma-highway-20".to_string()))
        );
    }

    #[test]
    fn a_merchant_account_run_with_no_parenthetical_is_never_a_candidate() {
        let intake_runs = vec![intake("intake-solo", "Solo Storage - QMS Onboarding")];
        let merchant_account_runs = vec![ma("ma-solo", "Solo Owner LLC")];

        let correlated = correlate_by_title(&intake_runs, &merchant_account_runs);

        assert!(correlated.is_empty());
    }

    #[test]
    fn distinct_intake_runs_correlate_independently() {
        let intake_runs = vec![
            intake("intake-highway-20", "Highway 20 Self Storage - QMS Onboarding"),
            intake("intake-pyott-road", "Pyott Road Self Storage - QMS Onboarding"),
        ];
        let merchant_account_runs = vec![
            ma("ma-highway-20", "Prairie Enterprises (Highway 20)"),
            ma("ma-pyott-road", "Prairie Enterprises (Pyott Road)"),
        ];

        let correlated = correlate_by_title(&intake_runs, &merchant_account_runs);

        assert_eq!(correlated.len(), 2);
    }
}
