//! Fuzzy matching of an Orchestrator facility to a ClickUp list.
//!
//! Names differ between the two systems (ClickUp lists are named by
//! whoever created them), so an exact comparison is useless. The shape
//! that proved out against the 30 real facilities in the dev database
//! and the live 816-list onboarding space (all 30 correct at top-1):
//!
//! * **Rare words count for more than common ones.** Tokens are weighted
//!   by inverse document frequency across every candidate list name, so
//!   "affordable" (in a dozen lists) matters little while "thibodaux" or
//!   "copperfield" decides the match. This is what makes
//!   "Affordable Storage, LLC" (Thibodaux) land on
//!   "Affordable Storage (Thibodaux, LA)" and not on the unrelated
//!   "Affordable Self Storage".
//! * **The facility's city counts as part of the query.** List names
//!   often carry a town; the facility name often doesn't.
//! * **The folder is only a hint.** A folder usually holds a company but
//!   is sometimes named for its legal entity ("1659 Birchwood LLC" holds
//!   "Northwest Heated Mini-Storage"), so the company-name-to-folder
//!   similarity is a small bonus, never the primary signal.
//! * **Generic words are dropped** ("self", "storage", "mini", "llc",
//!   ...) before comparing, since nearly every candidate shares them.
//!
//! Lists that are not facilities (Post-/Pre-Onboarding, templates,
//! sandboxes, "General / Multiple Sites", ...) are excluded outright --
//! Boris's call, 2026-10-02: they will never be wanted.
//!
//! The scores are only ever *suggestions*: a person confirms every link
//! before it is saved, and `confidence` exists so the confirm screen can
//! flag the rows that most need a second look.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

/// Words that appear in so many facility and list names that matching on
/// them says nothing.
const STOP_WORDS: &[&str] = &[
    "self",
    "storage",
    "mini",
    "llc",
    "inc",
    "co",
    "company",
    "the",
    "of",
    "and",
    "units",
    "unit",
    "rv",
    "boat",
    "climate",
    "controlled",
    "secure",
    "safe",
    "lp",
    "ltd",
    "properties",
    "management",
    "group",
    "ministorage",
    "stor",
    "storages",
    "warehouse",
];

/// Legal-entity suffixes, stripped before comparing a company name to a
/// folder name (so "Foo LLC" and "Foo, Inc." still read as the same).
const LEGAL_SUFFIXES: &[&str] = &[
    "llc",
    "inc",
    "lp",
    "ltd",
    "co",
    "corp",
    "company",
    "incorporated",
];

/// Below this, a candidate is not even worth suggesting.
pub const MIN_SUGGESTION_SCORE: f64 = 0.45;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Strong, unambiguous match.
    High,
    /// Probably right, worth a glance.
    Medium,
    /// A guess -- the row is left for the person to decide.
    Low,
}

/// A ClickUp list as a match candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    pub list_id: String,
    pub list_name: String,
    pub folder_id: String,
    pub folder_name: String,
}

/// What we know about the facility being matched.
#[derive(Debug, Clone, Copy)]
pub struct Query<'a> {
    pub facility_name: &'a str,
    pub city: Option<&'a str>,
    /// The company's legal name and DBA, used only for the small folder
    /// bonus.
    pub company_names: &'a [&'a str],
}

#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    pub entry: ListEntry,
    pub score: f64,
    pub confidence: Confidence,
}

fn normalize_tokens(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace('&', " and ")
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Distinctive tokens: everything except generic words. A name made
/// *only* of generic words ("Self Storage") falls back to all of its
/// tokens rather than matching nothing.
fn core_tokens(text: &str) -> Vec<String> {
    let all = normalize_tokens(text);
    let core: Vec<String> = all
        .iter()
        .filter(|token| !STOP_WORDS.contains(&token.as_str()))
        .cloned()
        .collect();

    if core.is_empty() {
        all
    } else {
        core
    }
}

fn without_legal_suffixes(text: &str) -> String {
    normalize_tokens(text)
        .into_iter()
        .filter(|token| !LEGAL_SUFFIXES.contains(&token.as_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn bigrams(text: &str) -> HashMap<(char, char), usize> {
    let chars: Vec<char> = text.chars().collect();
    let mut counts = HashMap::new();
    for pair in chars.windows(2) {
        *counts.entry((pair[0], pair[1])).or_insert(0) += 1;
    }
    counts
}

/// Sorensen-Dice similarity over character bigrams, 0.0..=1.0. Forgiving
/// of typos and word-order noise ("LCC" vs "LLC", "Hide-Away").
fn dice(a: &str, b: &str) -> f64 {
    let (ba, bb) = (bigrams(a), bigrams(b));
    let (total_a, total_b): (usize, usize) = (ba.values().sum(), bb.values().sum());
    if total_a == 0 || total_b == 0 {
        return 0.0;
    }

    let shared: usize = ba
        .iter()
        .map(|(pair, count)| (*count).min(*bb.get(pair).unwrap_or(&0)))
        .sum();

    2.0 * shared as f64 / (total_a + total_b) as f64
}

/// Whether a ClickUp list name is something other than a facility's
/// onboarding list. Compared on the name with everything but letters and
/// digits removed, so "Post-Onboarding", "Post On-Boarding" and
/// "post onboarding" are all the same to it.
pub fn is_facility_list(list_name: &str) -> bool {
    let squashed: String = list_name
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();

    const STARTS_WITH: &[&str] = &[
        "postonboarding",
        "preonboarding",
        "zzz",
        "general",
        "deliverables",
    ];
    const CONTAINS: &[&str] = &[
        "template",
        "sandbox",
        "sndbx",
        "training",
        "datespecific",
        "multiplesites",
        "multiplelocations",
        "allsites",
        "demoquestions",
    ];

    !(STARTS_WITH
        .iter()
        .any(|prefix| squashed.starts_with(prefix))
        || CONTAINS.iter().any(|needle| squashed.contains(needle)))
}

struct Indexed {
    entry: ListEntry,
    tokens: HashSet<String>,
    core_text: String,
    folder_text: String,
}

/// The candidate lists plus the corpus statistics that weight tokens.
/// Built once per hierarchy fetch and reused for every facility being
/// matched.
pub struct MatchIndex {
    entries: Vec<Indexed>,
    document_frequency: HashMap<String, usize>,
}

impl MatchIndex {
    /// Builds the index from every list, silently dropping the ones that
    /// are not facilities (see [`is_facility_list`]).
    pub fn new(lists: impl IntoIterator<Item = ListEntry>) -> Self {
        let entries: Vec<Indexed> = lists
            .into_iter()
            .filter(|entry| is_facility_list(&entry.list_name))
            .map(|entry| {
                let core = core_tokens(&entry.list_name);
                Indexed {
                    core_text: core.join(" "),
                    tokens: core.into_iter().collect(),
                    folder_text: without_legal_suffixes(&entry.folder_name),
                    entry,
                }
            })
            .collect();

        let mut document_frequency: HashMap<String, usize> = HashMap::new();
        for indexed in &entries {
            for token in &indexed.tokens {
                *document_frequency.entry(token.clone()).or_insert(0) += 1;
            }
        }

        Self {
            entries,
            document_frequency,
        }
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Inverse document frequency: a token in few list names is worth
    /// much more than one in many.
    fn idf(&self, token: &str) -> f64 {
        let n = self.entries.len() as f64;
        let df = *self.document_frequency.get(token).unwrap_or(&0) as f64;
        (1.0 + n / (1.0 + df)).ln()
    }

    fn score(
        &self,
        query_tokens: &HashSet<String>,
        query_core: &str,
        company_texts: &[String],
        candidate: &Indexed,
    ) -> f64 {
        let shared: f64 = query_tokens
            .intersection(&candidate.tokens)
            .map(|token| self.idf(token))
            .sum();
        let total: f64 = query_tokens
            .union(&candidate.tokens)
            .map(|token| self.idf(token))
            .sum();

        let weighted_jaccard = if total > 0.0 { shared / total } else { 0.0 };
        let name_score = 0.6 * weighted_jaccard + 0.4 * dice(query_core, &candidate.core_text);

        let folder_bonus = company_texts
            .iter()
            .map(|company| dice(company, &candidate.folder_text))
            .fold(0.0, f64::max);

        name_score + 0.25 * folder_bonus
    }

    /// The best `limit` candidates for `query`, best first, with scores
    /// below [`MIN_SUGGESTION_SCORE`] dropped.
    pub fn rank(&self, query: &Query, limit: usize) -> Vec<Match> {
        let core = core_tokens(query.facility_name);
        let query_core = core.join(" ");

        let mut query_tokens: HashSet<String> = core.into_iter().collect();
        if let Some(city) = query.city {
            query_tokens.extend(normalize_tokens(city));
        }

        let company_texts: Vec<String> = query
            .company_names
            .iter()
            .map(|name| without_legal_suffixes(name))
            .filter(|text| !text.is_empty())
            .collect();

        let mut scored: Vec<(f64, &Indexed)> = self
            .entries
            .iter()
            .map(|candidate| {
                (
                    self.score(&query_tokens, &query_core, &company_texts, candidate),
                    candidate,
                )
            })
            .filter(|(score, _)| *score >= MIN_SUGGESTION_SCORE)
            .collect();

        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored.truncate(limit);

        let runner_up = scored.get(1).map(|(score, _)| *score);

        scored
            .iter()
            .enumerate()
            .map(|(rank, (score, candidate))| Match {
                entry: candidate.entry.clone(),
                score: *score,
                // Only the top candidate's margin over the next one says
                // anything about how unambiguous it is.
                confidence: confidence_for(*score, if rank == 0 { runner_up } else { None }),
            })
            .collect()
    }

    /// The single best candidate, if any clears the minimum.
    #[cfg(test)]
    pub fn best(&self, query: &Query) -> Option<Match> {
        self.rank(query, 2).into_iter().next()
    }
}

/// High needs both a strong score and clear daylight over the runner-up;
/// a close second place is exactly the "which of these two?" case a
/// person should look at.
fn confidence_for(score: f64, runner_up: Option<f64>) -> Confidence {
    let margin = runner_up.map(|second| score - second).unwrap_or(1.0);

    if score >= 0.9 && margin >= 0.15 {
        Confidence::High
    } else if score >= 0.6 && margin >= 0.05 {
        Confidence::Medium
    } else {
        Confidence::Low
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(folder: &str, list: &str) -> ListEntry {
        ListEntry {
            list_id: format!("id-{list}"),
            list_name: list.to_string(),
            folder_id: format!("fid-{folder}"),
            folder_name: folder.to_string(),
        }
    }

    /// A trimmed slice of the real QMS Onboarding hierarchy (2026-10-02):
    /// the confusable names that actually exist, not invented ones.
    fn real_hierarchy() -> MatchIndex {
        let mut lists = Vec::new();
        for list in [
            "Affordable Storage Copperfield",
            "Affordable Storage Fadeway",
            "Affordable Storage FM 529",
            "Affordable Storage Katy-Flewellen",
            "Affordable Storage Lozano",
            "Affordable Storage Synott",
            "Affordable Storage Tanner",
            "Affordable Storage Westpark",
            "Affordable Storage Woodedge",
        ] {
            lists.push(entry("Affordable Storage - Beau Ryan", list));
        }
        lists.push(entry(
            "Affordable Storage, LLC",
            "Affordable Storage (Thibodaux, LA)",
        ));
        lists.push(entry(" Affordable Self Storage", "Affordable Self Storage"));
        lists.push(entry(
            "Affordable Multi-Storage Inc.",
            "Affordable Multi-Storage Inc.",
        ));
        lists.push(entry(
            "❌Affordable Storage - Katex",
            "Affordable Storage - Katex",
        ));
        lists.push(entry(
            "Dubuqueland Mini Storage, Inc.",
            "Dubuqueland Mini Storage",
        ));
        lists.push(entry(
            "Dubuqueland Mini Storage, Inc.",
            "Dubuqueland Mini Storage Parking",
        ));
        lists.push(entry(
            "Dubuqueland Mini Storage, Inc.",
            "Key West Mini Storage ",
        ));
        lists.push(entry(
            "Dubuqueland Mini Storage, Inc.",
            "Post-Onboarding - Dubuqueland",
        ));
        lists.push(entry(
            "Prairie Enterprises, LLC",
            "Carpentersville Self Storage",
        ));
        lists.push(entry("Prairie Enterprises, LLC", "Highway 20 Self Storage"));
        lists.push(entry("Prairie Enterprises, LLC", "Pyott Road Self Storage"));
        lists.push(entry(
            "Enterprise Self Storage",
            "Enterprise Self Storage - Sun Valley",
        ));
        lists.push(entry(
            "Occidental Holdings, Inc.",
            "Airport Road Self Storage",
        ));
        lists.push(entry("Pankey Properties", "Overton Road Self Storage"));
        lists.push(entry("MSS Jenks", "Main Street Storage - Jenks"));
        lists.push(entry("Absolute Management", "SC | Main Street Storage"));
        lists.push(entry("Union Street Storage", "Union Street Storage"));
        lists.push(entry("East 630 LLC", "East Avenue Storage"));
        lists.push(entry("Advance Management, Inc", "East Valley Storage"));
        lists.push(entry("Sprenger & Sprenger", "Moana Mini Storage"));
        lists.push(entry("Marine Team Storage", "Marine Team Storage"));
        lists.push(entry("Bass Storage", "Bass Storage"));
        lists.push(entry(
            "Bassett Self Storage LLC",
            "Bassett Self Storage LLC",
        ));
        lists.push(entry(
            "Toy Yard RV and Boat Storage, LLC",
            "Toy Yard RV and Boat Storage, LLC",
        ));
        lists.push(entry("10th and M Seafoods", "10th and M Seafoods"));
        lists.push(entry(
            "Hide-Away Storage INC",
            "Hideaway Self Storage - M Street",
        ));
        lists.push(entry(
            "Self Stor Mini Storages of Milton Freewater",
            "Self Stor Mini Storages of Milton Freewater",
        ));
        lists.push(entry("1659 Birchwood LLC", "Northwest Heated Mini-Storage"));
        MatchIndex::new(lists)
    }

    fn best_list(
        index: &MatchIndex,
        facility: &str,
        city: &str,
        company: &[&str],
    ) -> Option<(String, Confidence)> {
        index
            .best(&Query {
                facility_name: facility,
                city: Some(city),
                company_names: company,
            })
            .map(|m| (m.entry.list_name, m.confidence))
    }

    #[test]
    fn non_facility_lists_are_excluded_outright() {
        for name in [
            "Post-Onboarding - A-1 Storage",
            "Post On-Boarding Support",
            "Post Onboarding Training w/Brandon",
            "Pre-Onboarding Training",
            "Pre-Onboarding Demo Questions",
            "🎈 Absolute Management Template",
            "SANDBOX Additional Mini Storage",
            "SNDBX Woodinville Heated Storage",
            "zzzEverett Storage Depot",
            "GENERAL/Multiple Sites",
            "General / All Sites",
            "!Trojan - Multiple Sites",
            "Corporate / Multiple Locations",
            "General/Post-Onboarding",
            "Date Specific Tasks for Absolute",
        ] {
            assert!(!is_facility_list(name), "{name:?} must be excluded");
        }

        for name in [
            "Affordable Storage Synott",
            "Northwest Heated Mini-Storage",
            "Main Street Storage - Jenks",
            "A-OK Self Storage",
            "Greenfield Self Storage",
        ] {
            assert!(is_facility_list(name), "{name:?} must be kept");
        }
    }

    #[test]
    fn excluded_lists_never_appear_as_candidates() {
        let index = real_hierarchy();
        let all = index.rank(
            &Query {
                facility_name: "Dubuqueland Mini Storage",
                city: None,
                company_names: &[],
            },
            50,
        );

        assert!(all
            .iter()
            .all(|m| !m.entry.list_name.starts_with("Post-Onboarding")));
    }

    #[test]
    fn every_affordable_storage_facility_matches_its_own_list_with_high_confidence() {
        let index = real_hierarchy();

        for (facility, city) in [
            ("Affordable Storage Copperfield", "Houston"),
            ("Affordable Storage FM 529", "Cypress"),
            ("Affordable Storage Fadeway", "Houston"),
            ("Affordable Storage Katy-Flewellen", "Katy"),
            ("Affordable Storage Lozano", "Houston"),
            ("Affordable Storage Synott", "Houston"),
            ("Affordable Storage Tanner", "Houston"),
            ("Affordable Storage Westpark", "Houston"),
            ("Affordable Storage Woodedge", "Houston"),
        ] {
            let (list, confidence) =
                best_list(&index, facility, city, &["Affordable Storage"]).expect("a match");
            assert_eq!(list, facility);
            assert_eq!(confidence, Confidence::High, "{facility}");
        }
    }

    #[test]
    fn the_city_beats_a_lookalike_list_that_has_no_town() {
        // Real case: the facility is named just "Affordable Storage, LLC"
        // but the right list is "Affordable Storage (Thibodaux, LA)". A
        // bare name match would pick "Affordable Self Storage".
        let index = real_hierarchy();

        let (list, confidence) = best_list(
            &index,
            "Affordable Storage, LLC",
            "Thibodaux",
            &["Affordable Storage, LLC"],
        )
        .expect("a match");

        assert_eq!(list, "Affordable Storage (Thibodaux, LA)");
        // Only narrowly ahead of the lookalike -- must not claim certainty.
        assert_ne!(confidence, Confidence::High);
    }

    #[test]
    fn a_folder_named_for_the_legal_entity_does_not_stop_the_facility_match() {
        let index = real_hierarchy();

        let (list, _) = best_list(
            &index,
            "East Avenue Storage",
            "Rochester",
            &["East 630 LLC"],
        )
        .expect("a match");
        assert_eq!(list, "East Avenue Storage");

        let (list, _) = best_list(
            &index,
            "Moana Mini Storage",
            "Reno",
            &["Sprenger & Sprenger"],
        )
        .expect("a match");
        assert_eq!(list, "Moana Mini Storage");
    }

    #[test]
    fn a_typo_in_the_facility_name_still_matches() {
        let index = real_hierarchy();

        let (list, _) = best_list(
            &index,
            "Toy Yard RV and Boat Storage, LCC",
            "Whitwell",
            &["Toy Yard RV and Boat Storage, LLC"],
        )
        .expect("a match");

        assert_eq!(list, "Toy Yard RV and Boat Storage, LLC");
    }

    #[test]
    fn reordered_and_abbreviated_names_match() {
        let index = real_hierarchy();

        let (list, _) =
            best_list(&index, "Main Street Storage", "Jenks", &["MSS Jenks"]).expect("a match");
        // Both "Main Street Storage - Jenks" and "SC | Main Street Storage"
        // contain the name; the city plus the MSS Jenks folder decide.
        assert_eq!(list, "Main Street Storage - Jenks");

        let (list, _) = best_list(
            &index,
            "10th & M Seafoods",
            "Anchorage",
            &["10th & M Seafoods"],
        )
        .expect("a match");
        assert_eq!(list, "10th and M Seafoods");
    }

    #[test]
    fn a_genuinely_ambiguous_facility_is_not_claimed_with_high_confidence() {
        let index = real_hierarchy();

        // "Upper Lot" has no list of its own; the best the data offers is
        // the main facility's list, which a person must confirm.
        let (_, confidence) = best_list(
            &index,
            "Dubuqueland Mini Storage - Upper Lot",
            "Peosta",
            &["Dubuqueland Mini-Storage, Inc."],
        )
        .expect("a match");

        assert_ne!(confidence, Confidence::High);
    }

    #[test]
    fn nothing_is_suggested_when_nothing_is_close() {
        let index = real_hierarchy();

        let result = index.best(&Query {
            facility_name: "Zebra Crossing Vault",
            city: Some("Nowhere"),
            company_names: &["Unrelated Holdings"],
        });

        assert!(result.is_none());
    }

    #[test]
    fn ranking_returns_best_first_and_respects_the_limit() {
        let index = real_hierarchy();
        let query = Query {
            facility_name: "Dubuqueland Mini Storage",
            city: None,
            company_names: &[],
        };

        // Two lists plausibly match (the main facility and its parking).
        let all = index.rank(&query, 50);
        assert!(
            all.len() >= 2,
            "expected several plausible lists, got {}",
            all.len()
        );
        assert!(all.windows(2).all(|pair| pair[0].score >= pair[1].score));

        let limited = index.rank(&query, 1);
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].entry.list_id, all[0].entry.list_id);
        assert_eq!(all[0].entry.list_name, "Dubuqueland Mini Storage");
    }

    #[test]
    fn an_empty_hierarchy_suggests_nothing_and_does_not_panic() {
        let index = MatchIndex::new(Vec::new());

        assert!(index.is_empty());
        assert!(index
            .best(&Query {
                facility_name: "Anything",
                city: None,
                company_names: &[],
            })
            .is_none());
    }

    #[test]
    fn a_name_made_only_of_generic_words_still_matches_by_those_words() {
        let index = MatchIndex::new(vec![entry("X", "Self Storage")]);

        let result = index.best(&Query {
            facility_name: "Self Storage",
            city: None,
            company_names: &[],
        });

        assert!(result.is_some());
    }

    #[test]
    fn dice_is_symmetric_and_bounded() {
        assert!((dice("night", "nacht") - dice("nacht", "night")).abs() < 1e-12);
        assert_eq!(dice("same", "same"), 1.0);
        assert_eq!(dice("", "x"), 0.0);
    }
}
