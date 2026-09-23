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
//!
//! **2026-09-17: added a second signal, the run's own `Business_DBA`
//! field.** The title's parenthetical is a human-typed, best-effort
//! proxy for the same identifying information the Merchant Account
//! form already captures cleanly in `Business_DBA` -- and PS's own
//! naming convention isn't universal: a real Merchant Account run can
//! be titled plainly (`"<name> - New Elavon Account"`, no parens at
//! all), which `parenthetical()` can never extract anything from,
//! regardless of how specific that name is. Real case: Main Street
//! Storage's own completed application never correlated to anything
//! by title (confirmed missed a 2-week window this way) while its
//! `Business_DBA` ("Main Street Storage") matches its Intake facility
//! name exactly. `Business_DBA` is additive, not a replacement --
//! either signal alone is enough to make a run a candidate, and a run
//! with a specific parenthetical AND a specific DBA that disagree on
//! *which* Intake run they point to still correctly surfaces as
//! `Correlation::Ambiguous` rather than picking one silently.
//!
//! **Known blind spot, confirmed against real data, not yet solved
//! here**: `Business_DBA` itself can legitimately differ from the
//! Intake facility name -- e.g. a management company's own internal
//! name for a property vs. the name a sales rep typed into Intake.
//! Absolute Storage Management's own conversions are the clearest
//! examples of this seen so far (**note: Absolute is not a
//! representative example of client data generally -- see
//! `[[Gotchas]]`'s own note on this; Prairie Enterprises, Dubuqueland,
//! and Affordable Storage (Beau Ryan) are the good reference cases**),
//! and also submit via a PDF-import path that leaves every owner/
//! signer/address field blank, so there's no fallback signal to try
//! either. Neither signal here is expected to solve that subset; it
//! remains a manual-link case.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

/// One locally-indexed Merchant Account run's own identity.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MerchantAccountRunInfo {
    pub run_id: String,
    pub run_name: String,
    /// This run's own `Business_DBA` form field (falling back to the
    /// `Facility_Name_in_CRM`/`Facility_Name_in_Zoho` key-drift variants
    /// -- see `sync::orchestrator::sync_one_run`'s own extraction),
    /// persisted at sync time so this stays a purely-local lookup. A
    /// second, more direct correlation signal alongside the run's own
    /// title -- see `correlate_by_title`'s own doc comment for why
    /// this was added 2026-09-17.
    pub business_dba: Option<String>,
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
        "SELECT ps_run_id AS run_id, run_name, business_dba, ps_updated_at AS updated_at
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

/// A street-type word's real observed variants, mapped to one
/// canonical short form -- "Av.", "Ave.", and "Avenue" all describe the
/// same street type but compare unequal as plain text. Not exhaustive,
/// just the types actually seen in real PS address data plus the
/// obvious rest; extend as new ones turn up rather than trying to
/// enumerate the whole USPS suffix list up front.
const STREET_TYPE_ALIASES: &[(&str, &[&str])] = &[
    ("ave", &["av", "aven", "avenue", "avenu"]),
    ("blvd", &["boul", "boulevard"]),
    ("st", &["str", "street"]),
    ("dr", &["driv", "drive"]),
    ("rd", &["road"]),
    ("ln", &["lane"]),
    ("ct", &["court"]),
    ("pl", &["place"]),
    ("hwy", &["highway"]),
    ("pkwy", &["pky", "parkway"]),
    ("cir", &["circle"]),
    ("ste", &["suite"]),
    ("apt", &["apartment"]),
];

/// Normalizes a business address for loose comparison -- lowercases,
/// drops punctuation, and canonicalizes street-type words via
/// `STREET_TYPE_ALIASES` (Boris, 2026-09-23: "fuzzy only in case things
/// don't match exactly, e.g. av. vs. ave. vs. avenue"). Not a real
/// address-parsing/geocoding normalization -- just enough to stop
/// formatting noise from registering as a real difference between two
/// humans typing the same address.
fn normalize_address(address: &str) -> String {
    let stripped: String = address
        .chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() { c } else { ' ' })
        .collect();

    stripped
        .split_whitespace()
        .map(|word| {
            let lower = word.to_lowercase();
            STREET_TYPE_ALIASES
                .iter()
                .find(|(canonical, variants)| *canonical == lower || variants.contains(&lower.as_str()))
                .map(|(canonical, _)| canonical.to_string())
                .unwrap_or(lower)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Loosely compares two business addresses -- see `normalize_address`.
/// Used to tell a manager whether every candidate in a "Potential
/// Duplicates" group shares one real address (consistent with a genuine
/// duplicate submission of the same application) or not (consistent
/// with two different real businesses that merely share similar-looking
/// titles -- the Knapp's Self Stor of Milton Freewater / "Milton Self
/// Storage" mix-up). Decision support only -- never used to silently
/// resolve an `Ambiguous` correlation on its own.
pub fn addresses_fuzzy_match(a: &str, b: &str) -> bool {
    normalize_address(a) == normalize_address(b)
}

/// Generic words that appear in enough real facility/business titles to
/// be worthless as a "these two might be the same place" signal on
/// their own -- the same reasoning `is_specific_enough` already applies
/// to a single short nickname, extended to name-similarity checking.
const NAME_STOPWORDS: &[&str] = &[
    "self", "storage", "llc", "inc", "the", "qms", "onboarding", "new", "elavon", "account", "of",
    "mini", "and", "a", "for",
];

/// Splits a title into its significant (non-stopword, 2+ character)
/// lowercase words -- shared vocabulary for both `candidate_keywords`-
/// style exact matching and the looser near-miss check below.
fn significant_words(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.chars().count() >= 2 && !NAME_STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Whether two titles share at least one significant word, without
/// being the same title (case-insensitive) and without one substring-
/// containing the other -- a genuine name match or a real substring hit
/// is `correlate_by_title`'s own job; this exists for the *weaker*
/// signal one step below that: "these two are talking about different
/// things, probably, but they share enough vocabulary that a human
/// should double check before acting" (real case: "Milton Self Storage"
/// vs. "Knapp's Self Stor of Milton Freewater" -- share "milton", one
/// is not a substring of the other, and they are two different real
/// businesses).
pub fn shares_a_significant_word(a: &str, b: &str) -> bool {
    let a_lower = a.to_lowercase();
    let b_lower = b.to_lowercase();
    if a_lower == b_lower || a_lower.contains(&b_lower) || b_lower.contains(&a_lower) {
        return false;
    }

    !significant_words(a).is_disjoint(&significant_words(b))
}

/// Whether a parenthetical nickname is specific enough to trust as a
/// correlation signal on its own. **Real bug, 2026-09-10**: Dubuqueland
/// Mini Storage's own "Main" facility gave a Merchant Account run
/// titled `"...(Main)"`, and `"main"` is a plain substring of an
/// entirely unrelated new client's own Intake title, `"Main Street
/// Storage - QMS Onboarding"` -- correlated as `Unambiguous` (nothing
/// else competed for the slot) and silently seeded Dubuqueland's own
/// legal name onto Main Street Storage's confirmation screen. A
/// word-boundary check wouldn't have caught this: "Main" is a genuine
/// whole word in both titles. The real problem is that a short,
/// single-word nickname isn't a specific enough discriminator to
/// trust unattended, so it's excluded as a candidate entirely here --
/// same treatment as `parenthetical` returning `None`, i.e. it can
/// still surface as `Correlation::Ambiguous` if some OTHER, more
/// specific nickname also matches, but a lone short/generic nickname
/// no longer produces a false `Unambiguous`.
fn is_specific_enough(keyword: &str) -> bool {
    keyword.split_whitespace().count() >= 2 || keyword.chars().count() >= 6
}

/// Every candidate keyword one Merchant Account run could be found by
/// -- its own title's parenthetical nickname (the original signal) and
/// its own `business_dba` (added 2026-09-17), each independently
/// gated by `is_specific_enough`. Either, both, or neither may apply to
/// a given run; `correlate_by_title` doesn't need to know which
/// signal(s) actually fired, just that at least one did.
fn candidate_keywords(ma: &MerchantAccountRunInfo) -> Vec<&str> {
    let mut keywords = Vec::new();

    if let Some(nickname) = parenthetical(&ma.run_name) {
        if is_specific_enough(nickname) {
            keywords.push(nickname);
        }
    }

    if let Some(dba) = ma.business_dba.as_deref().map(str::trim) {
        if !dba.is_empty() && is_specific_enough(dba) {
            keywords.push(dba);
        }
    }

    keywords
}

/// Correlates each Intake run in `intake_runs` against every Merchant
/// Account run in `merchant_account_runs`, by checking whether any of
/// a Merchant Account run's own candidate keywords (see
/// `candidate_keywords`) appears (case-insensitive substring) inside
/// the Intake run's own title text. An Intake run matching zero
/// Merchant Account runs is simply absent from the result -- matching
/// two or more (whether via the same keyword or two different ones) is
/// surfaced as `Correlation::Ambiguous`, not silently dropped or
/// arbitrarily picked (see this module's own doc comment for why that
/// distinction matters for real data).
pub fn correlate_by_title(
    intake_runs: &[IntakeRunTitle],
    merchant_account_runs: &[MerchantAccountRunInfo],
) -> HashMap<String, Correlation> {
    let mut candidates: HashMap<&str, HashSet<&str>> = HashMap::new();

    for ma in merchant_account_runs {
        for keyword in candidate_keywords(ma) {
            let keyword_lower = keyword.to_lowercase();

            for intake in intake_runs {
                if intake.title_text.to_lowercase().contains(&keyword_lower) {
                    candidates
                        .entry(&intake.run_id)
                        .or_default()
                        .insert(ma.run_id.as_str());
                }
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

    #[test]
    fn addresses_fuzzy_match_despite_street_type_abbreviation_differences() {
        assert!(addresses_fuzzy_match(
            "123 Main Av.",
            "123 Main Avenue"
        ));
        assert!(addresses_fuzzy_match("123 Main Ave.", "123 Main Avenue"));
        assert!(addresses_fuzzy_match(
            "84097 Hwy 11, Milton Freewater, OR 97862",
            "84097 Highway 11, Milton Freewater, OR 97862"
        ));
    }

    #[test]
    fn addresses_fuzzy_match_is_case_and_punctuation_insensitive() {
        assert!(addresses_fuzzy_match(
            "123 Main St., Suite 4",
            "123 MAIN STREET STE 4"
        ));
    }

    #[test]
    fn addresses_fuzzy_match_is_false_for_genuinely_different_addresses() {
        assert!(!addresses_fuzzy_match(
            "84097 Hwy 11, Milton Freewater, OR 97862",
            "500 Elm St, Springfield, IL 62704"
        ));
    }

    // The real case this exists for: two different real businesses
    // whose titles both happen to contain "Milton", in different word
    // order, with neither a substring of the other -- must be flagged
    // as a near miss, not silently ignored the way a plain substring
    // check (`correlate_by_title`'s own signal) would.
    #[test]
    fn shares_a_significant_word_flags_the_real_milton_mix_up() {
        assert!(shares_a_significant_word(
            "Milton Self Storage",
            "Knapp's Self Stor of Milton Freewater"
        ));
    }

    #[test]
    fn shares_a_significant_word_is_false_for_the_same_title() {
        assert!(!shares_a_significant_word(
            "Highway 20 Self Storage",
            "Highway 20 Self Storage"
        ));
    }

    #[test]
    fn shares_a_significant_word_is_false_when_one_title_contains_the_other() {
        // A genuine substring relationship is `correlate_by_title`'s own
        // signal to act on -- not a "these might be different, double
        // check" near miss.
        assert!(!shares_a_significant_word(
            "Prairie Enterprises (Highway 20)",
            "Highway 20"
        ));
    }

    #[test]
    fn shares_a_significant_word_ignores_common_storage_industry_words() {
        // "Self" and "Storage" alone must never trigger a near-miss --
        // half of real client titles contain both.
        assert!(!shares_a_significant_word(
            "Highway 20 Self Storage",
            "Pyott Road Self Storage"
        ));
    }

    #[test]
    fn shares_a_significant_word_is_false_for_genuinely_unrelated_titles() {
        assert!(!shares_a_significant_word(
            "Highway 20 Self Storage",
            "Dubuqueland Mini Storage"
        ));
    }

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
            business_dba: None,
            updated_at: DateTime::UNIX_EPOCH,
        }
    }

    fn ma_with_dba(run_id: &str, run_name: &str, business_dba: &str) -> MerchantAccountRunInfo {
        MerchantAccountRunInfo {
            run_id: run_id.to_string(),
            run_name: run_name.to_string(),
            business_dba: Some(business_dba.to_string()),
            updated_at: DateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn parenthetical_extracts_the_facility_nickname() {
        assert_eq!(
            parenthetical("Prairie Enterprises (Highway 20)"),
            Some("Highway 20")
        );
    }

    #[test]
    fn parenthetical_is_none_without_a_real_facility_name_inside() {
        assert_eq!(parenthetical("Prairie Enterprises LLC"), None);
        assert_eq!(parenthetical("Empty Parens ()"), None);
    }

    #[test]
    fn short_single_word_nicknames_are_not_specific_enough() {
        assert!(!is_specific_enough("Main"));
        assert!(!is_specific_enough("West"));
    }

    #[test]
    fn a_longer_single_word_nickname_is_specific_enough() {
        assert!(is_specific_enough("Carpentersville"));
    }

    #[test]
    fn a_short_multi_word_nickname_is_specific_enough() {
        assert!(is_specific_enough("Pyott Rd"));
    }

    // Real bug, 2026-09-10: Dubuqueland Mini Storage's own "Main"
    // facility's Merchant Account run title contains "main", which is
    // also a plain substring of an entirely unrelated new client's own
    // Intake title -- must not correlate the two just because nothing
    // else happened to compete for the slot.
    #[test]
    fn a_short_generic_nickname_does_not_falsely_correlate_an_unrelated_facility() {
        let intake_runs = vec![intake(
            "intake-main-street-storage",
            "Main Street Storage - QMS Onboarding",
        )];
        let merchant_account_runs = vec![ma(
            "ma-dubuqueland-main",
            "Dubuqueland Mini-Storage, Inc. (Main)",
        )];

        let correlated = correlate_by_title(&intake_runs, &merchant_account_runs);

        assert!(!correlated.contains_key("intake-main-street-storage"));
    }

    // The real facility this nickname actually belongs to must still
    // correlate correctly when its own title is the one being checked
    // -- the guard only screens out short nicknames as candidates, it
    // doesn't break a run that's genuinely titled with the word "Main"
    // when a longer, more specific nickname is what's really being
    // matched elsewhere.
    #[test]
    fn a_specific_enough_nickname_still_correlates_normally_alongside_a_generic_one() {
        let intake_runs = vec![
            intake(
                "intake-main-street-storage",
                "Main Street Storage - QMS Onboarding",
            ),
            intake(
                "intake-highway-20",
                "Highway 20 Self Storage - QMS Onboarding",
            ),
        ];
        let merchant_account_runs = vec![
            ma(
                "ma-dubuqueland-main",
                "Dubuqueland Mini-Storage, Inc. (Main)",
            ),
            ma("ma-highway-20", "Prairie Enterprises (Highway 20)"),
        ];

        let correlated = correlate_by_title(&intake_runs, &merchant_account_runs);

        assert!(!correlated.contains_key("intake-main-street-storage"));
        assert_eq!(
            correlated.get("intake-highway-20"),
            Some(&Correlation::Unambiguous("ma-highway-20".to_string()))
        );
    }

    // Real Prairie Enterprises data, captured 2026-09-02 -- proves the
    // fix against the exact case that was broken, not a synthetic one.
    #[test]
    fn real_prairie_data_correlates_each_facility_to_its_own_merchant_account_run() {
        let intake_runs = vec![
            intake(
                "intake-highway-20",
                "Highway 20 Self Storage - QMS Onboarding",
            ),
            intake(
                "intake-carpentersville",
                "Carpentersville Self Storage - QMS Onboarding",
            ),
            intake(
                "intake-pyott-road",
                "Pyott Road Self Storage - QMS Onboarding",
            ),
        ];
        let merchant_account_runs = vec![
            ma("ma-highway-20", "Prairie Enterprises (Highway 20)"),
            ma(
                "ma-carpentersville-1",
                "Prairie Enterprises (Carpentersville)",
            ),
            ma(
                "ma-carpentersville-2",
                "Prairie Enterprises (Carpentersville)",
            ),
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
        let intake_runs = vec![intake(
            "intake-highway-20",
            "Highway 20 Self Storage - QMS Onboarding",
        )];
        let merchant_account_runs = vec![
            ma("ma-highway-20", "Prairie Enterprises (Highway 20)"),
            ma(
                "ma-carpentersville",
                "Prairie Enterprises (Carpentersville)",
            ),
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
            intake(
                "intake-highway-20",
                "Highway 20 Self Storage - QMS Onboarding",
            ),
            intake(
                "intake-pyott-road",
                "Pyott Road Self Storage - QMS Onboarding",
            ),
        ];
        let merchant_account_runs = vec![
            ma("ma-highway-20", "Prairie Enterprises (Highway 20)"),
            ma("ma-pyott-road", "Prairie Enterprises (Pyott Road)"),
        ];

        let correlated = correlate_by_title(&intake_runs, &merchant_account_runs);

        assert_eq!(correlated.len(), 2);
    }

    // Real bug, 2026-09-17: Main Street Storage's own completed
    // Merchant Account application ("Main Street Storage - New Elavon
    // Account") has no parenthetical at all, so it never correlated to
    // anything by title -- confirmed live, missed entirely until
    // caught by hand. Its own `Business_DBA` ("Main Street Storage")
    // matches its Intake facility name exactly, which the DBA signal
    // now catches on its own, with zero contribution from the title.
    #[test]
    fn a_business_dba_with_no_useful_parenthetical_still_correlates() {
        let intake_runs = vec![intake(
            "intake-main-street-storage",
            "Main Street Storage - QMS Onboarding",
        )];
        let merchant_account_runs = vec![ma_with_dba(
            "ma-main-street-storage",
            "Main Street Storage - New Elavon Account",
            "Main Street Storage",
        )];

        let correlated = correlate_by_title(&intake_runs, &merchant_account_runs);

        assert_eq!(
            correlated.get("intake-main-street-storage"),
            Some(&Correlation::Unambiguous(
                "ma-main-street-storage".to_string()
            ))
        );
    }

    #[test]
    fn a_short_generic_business_dba_is_not_specific_enough_either() {
        let intake_runs = vec![intake("intake-west", "West Self Storage - QMS Onboarding")];
        let merchant_account_runs = vec![ma_with_dba(
            "ma-west",
            "Some Owner LLC - New Elavon Account",
            "West",
        )];

        let correlated = correlate_by_title(&intake_runs, &merchant_account_runs);

        assert!(correlated.is_empty());
    }

    /// Both signals can independently find the same run for the same
    /// Intake run -- still just one `Unambiguous` match, not treated as
    /// a stronger or different kind of result. This module doesn't
    /// model confidence levels, only whether a run is a candidate at
    /// all.
    #[test]
    fn a_run_found_by_both_signals_at_once_is_still_a_single_unambiguous_match() {
        let intake_runs = vec![intake(
            "intake-highway-20",
            "Highway 20 Self Storage - QMS Onboarding",
        )];
        let merchant_account_runs = vec![ma_with_dba(
            "ma-highway-20",
            "Prairie Enterprises (Highway 20)",
            "Highway 20 self storage",
        )];

        let correlated = correlate_by_title(&intake_runs, &merchant_account_runs);

        assert_eq!(
            correlated.get("intake-highway-20"),
            Some(&Correlation::Unambiguous("ma-highway-20".to_string()))
        );
    }

    /// **Known limitation, confirmed here rather than silently
    /// "fixed"**: a single Merchant Account run whose two signals
    /// disagree -- its title's parenthetical points at one Intake run,
    /// its `Business_DBA` points at a *different* one -- currently
    /// resolves to `Unambiguous` for **both**, independently, since
    /// each Intake run only ever sees its own candidate count and has
    /// no way to know the same `ma.run_id` was also claimed elsewhere.
    /// This blind spot predates the DBA signal (the same thing happens
    /// today if one run's parenthetical alone happened to substring-
    /// match two different Intake titles) -- adding a second signal
    /// just made it easier to hit in practice, since it doubled the
    /// chances of exactly this disagreement. Not fixed in this pass;
    /// flagged for a real design discussion (e.g. a post-pass that
    /// demotes any `ma.run_id` claimed `Unambiguous` by more than one
    /// Intake run to `Ambiguous` for all of them) rather than guessed
    /// at here.
    #[test]
    fn disagreement_between_the_two_signals_independently_unambiguous_for_both_today() {
        let intake_runs = vec![
            intake(
                "intake-highway-20",
                "Highway 20 Self Storage - QMS Onboarding",
            ),
            intake(
                "intake-pyott-road",
                "Pyott Road Self Storage - QMS Onboarding",
            ),
        ];
        let merchant_account_runs = vec![ma_with_dba(
            "ma-mismatched",
            "Prairie Enterprises (Highway 20)",
            "Pyott Road self storage",
        )];

        let correlated = correlate_by_title(&intake_runs, &merchant_account_runs);

        assert_eq!(
            correlated.get("intake-highway-20"),
            Some(&Correlation::Unambiguous("ma-mismatched".to_string()))
        );
        assert_eq!(
            correlated.get("intake-pyott-road"),
            Some(&Correlation::Unambiguous("ma-mismatched".to_string()))
        );
    }
}
