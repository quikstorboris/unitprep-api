//! Turns each facility's ranked ClickUp candidates into one suggested
//! list per facility, **never suggesting the same list twice**.
//!
//! Ranking facilities independently can send two of them to one list --
//! e.g. "Dubuqueland Mini Storage, Inc." and "Dubuqueland Mini Storage -
//! Upper Lot" both rank "Dubuqueland Mini Storage" first. The default
//! relationship is one list per facility, so the facility with the
//! stronger claim keeps the list and the other falls back to its next
//! candidate (or to no suggestion, leaving the row for a person). A
//! person can still deliberately point two facilities at one list; this
//! only governs what is *suggested*.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use super::matching::Match;

/// `ranked` is each facility's candidates best-first. Facilities are
/// served strongest-claim first (highest top score; ties broken by key
/// order of appearance), each taking its best candidate that no stronger
/// facility already took.
pub fn assign_unique<K: Clone + Eq + Hash>(
    ranked: Vec<(K, Vec<Match>)>,
) -> HashMap<K, Option<Match>> {
    let mut order: Vec<usize> = (0..ranked.len()).collect();
    order.sort_by(|&a, &b| {
        let top = |i: usize| ranked[i].1.first().map(|m| m.score).unwrap_or(f64::MIN);
        top(b).total_cmp(&top(a))
    });

    let mut claimed: HashSet<String> = HashSet::new();
    let mut result = HashMap::new();

    for index in order {
        let (key, candidates) = &ranked[index];
        let pick = candidates
            .iter()
            .find(|candidate| !claimed.contains(&candidate.entry.list_id))
            .cloned();

        if let Some(chosen) = &pick {
            claimed.insert(chosen.entry.list_id.clone());
        }
        result.insert(key.clone(), pick);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clickup::matching::{Confidence, ListEntry};

    fn candidate(list_id: &str, score: f64) -> Match {
        Match {
            entry: ListEntry {
                list_id: list_id.to_string(),
                list_name: list_id.to_string(),
                folder_id: "f".to_string(),
                folder_name: "f".to_string(),
            },
            score,
            confidence: Confidence::Medium,
        }
    }

    #[test]
    fn each_facility_gets_its_own_best_list_when_there_is_no_conflict() {
        let result = assign_unique(vec![
            ("a", vec![candidate("L1", 0.9)]),
            ("b", vec![candidate("L2", 0.8)]),
        ]);

        assert_eq!(result["a"].as_ref().unwrap().entry.list_id, "L1");
        assert_eq!(result["b"].as_ref().unwrap().entry.list_id, "L2");
    }

    #[test]
    fn the_stronger_claim_keeps_a_contested_list_and_the_other_falls_back() {
        // Both want L1; "main" claims it more strongly, so "upper-lot"
        // takes its second choice.
        let result = assign_unique(vec![
            (
                "upper-lot",
                vec![candidate("L1", 0.65), candidate("L2", 0.59)],
            ),
            ("main", vec![candidate("L1", 0.92), candidate("L2", 0.72)]),
        ]);

        assert_eq!(result["main"].as_ref().unwrap().entry.list_id, "L1");
        assert_eq!(result["upper-lot"].as_ref().unwrap().entry.list_id, "L2");
    }

    #[test]
    fn a_loser_with_no_other_candidate_is_left_without_a_suggestion() {
        let result = assign_unique(vec![
            ("weak", vec![candidate("L1", 0.5)]),
            ("strong", vec![candidate("L1", 0.95)]),
        ]);

        assert!(result["strong"].is_some());
        assert!(result["weak"].is_none());
    }

    #[test]
    fn a_facility_with_no_candidates_gets_none_and_does_not_block_others() {
        let result = assign_unique(vec![
            ("nothing", Vec::new()),
            ("something", vec![candidate("L1", 0.7)]),
        ]);

        assert!(result["nothing"].is_none());
        assert!(result["something"].is_some());
    }

    #[test]
    fn no_list_is_ever_suggested_twice() {
        let result = assign_unique(vec![
            (
                "a",
                vec![
                    candidate("L1", 0.9),
                    candidate("L2", 0.8),
                    candidate("L3", 0.7),
                ],
            ),
            (
                "b",
                vec![
                    candidate("L1", 0.85),
                    candidate("L2", 0.8),
                    candidate("L3", 0.7),
                ],
            ),
            (
                "c",
                vec![
                    candidate("L1", 0.8),
                    candidate("L2", 0.79),
                    candidate("L3", 0.78),
                ],
            ),
        ]);

        let mut lists: Vec<String> = result
            .values()
            .flatten()
            .map(|m| m.entry.list_id.clone())
            .collect();
        lists.sort();
        lists.dedup();
        assert_eq!(lists.len(), 3);
    }
}
