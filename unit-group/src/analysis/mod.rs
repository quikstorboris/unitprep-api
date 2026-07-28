// Analysis pipeline entry point: takes a batch of per-facility group
// inventories plus an optional reference (master) group list, and produces
// net-new groups (exact-match only — see fingerprint.rs and the project
// history for why exact matching is load-bearing here) and advisory
// similarity warnings (fingerprint-gated fuzzy matching, informational
// only — never affects net-new determination).

mod batch;
pub mod fingerprint;
mod reference;

pub use batch::build_batch_from_documents;
pub use fingerprint::{
    has_malformed_dimension_attempt, is_uncommon_group_name, parse_fingerprint, Climate,
    GroupFingerprint, Location,
};
pub use reference::{load_reference_groups_from_document, select_group_document};

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use strsim::normalized_levenshtein;

use fingerprint::fingerprints_match;

use crate::models::{AdvisoryIssue, AnalysisResults, BatchRun, Severity, SimilarityMatch};

pub fn analyze_batch(
    batch: BatchRun,
    reference_groups: Option<Vec<String>>,
) -> Result<AnalysisResults> {
    // `batch` is owned here, not borrowed — destructuring it moves each
    // field out directly instead of cloning a batch that's about to be
    // dropped anyway.
    let BatchRun {
        facilities: facility_groups,
        global_groups,
        advisory_issues: mut issues,
    } = batch;

    let mut net_new_groups = Vec::new();

    let mut similar_groups = Vec::new();

    if let Some(reference_groups) = &reference_groups {
        let reference_set: HashSet<_> = reference_groups.iter().cloned().collect();

        let parsed_reference_groups = reference_groups
            .iter()
            .map(|group| (group.clone(), parse_fingerprint(group)))
            .collect::<Vec<_>>();

        // A net-new group name recurring across many facilities is the
        // normal case in a multi-facility batch, not an edge case — collect
        // every (facility, group) occurrence first, alongside the distinct
        // set of group names, so the fingerprint match below is computed
        // once per *name*, not once per facility that happens to have it.
        let mut occurrences: Vec<(String, String)> = Vec::new();

        let mut distinct_net_new: HashSet<String> = HashSet::new();

        for facility in &facility_groups {
            for group in facility.groups.keys() {
                if reference_set.contains(group) {
                    continue;
                }

                distinct_net_new.insert(group.clone());

                occurrences.push((facility.name.clone(), group.clone()));
            }
        }

        net_new_groups.extend(distinct_net_new.iter().cloned());

        // Cache keyed by group name, not by (facility, group) — the best
        // reference match only ever depends on the group name itself, so
        // computing it once per distinct name and reusing the result both
        // fixes `similar_groups` getting one literal duplicate entry per
        // facility (it has no facility field to distinguish them by) and
        // avoids redoing the same O(reference_groups) scan for every
        // facility that shares a group name.
        let best_match_cache: HashMap<String, Option<(String, f64)>> = distinct_net_new
            .iter()
            .map(|group| {
                let parsed_group = parse_fingerprint(group);

                let mut best_match = None;
                let mut best_score = 0.0_f64;

                for (candidate, candidate_fp) in &parsed_reference_groups {
                    if !fingerprints_match(&parsed_group, candidate_fp) {
                        continue;
                    }

                    let score =
                        normalized_levenshtein(&parsed_group.remainder, &candidate_fp.remainder);

                    if score > best_score {
                        best_score = score;
                        best_match = Some(candidate.clone());
                    }
                }

                (
                    group.clone(),
                    best_match.map(|candidate| (candidate, best_score)),
                )
            })
            .collect();

        // One SimilarityMatch per distinct net-new group name — this feeds
        // the UI-facing "Similar Groups" summary, which should show each
        // odd group once, not once per facility it happens to appear in.
        for group in &distinct_net_new {
            let Some(Some((candidate, score))) = best_match_cache.get(group) else {
                continue;
            };

            if *score >= 0.80 && candidate != group {
                similar_groups.push(SimilarityMatch {
                    facility_group: group.clone(),
                    reference_group: candidate.clone(),
                    similarity: *score,
                    difference: format!("{} -> {}", group, candidate),
                });
            }
        }

        // One advisory Issue per facility occurrence — unlike
        // `similar_groups` above, this is genuinely per-facility
        // information (which facility's export has the odd group), so it
        // isn't deduplicated the same way; it's just computed from the
        // cache instead of recomputed per occurrence.
        for (facility_name, group) in &occurrences {
            let Some(Some((candidate, score))) = best_match_cache.get(group) else {
                continue;
            };

            if *score >= 0.80 && candidate != group {
                issues.push(AdvisoryIssue {
                    source: format!("Facility {}", facility_name),
                    issue: format!(
                        "Similar but not exact match found: '{}' vs '{}' (score {:.2})",
                        group, candidate, score
                    ),
                    severity: Severity::Warning,
                });
            }
        }
    } else {
        net_new_groups.extend(global_groups.keys().cloned());
    }

    net_new_groups.sort();
    net_new_groups.dedup();

    similar_groups.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(AnalysisResults {
        batch_run: BatchRun {
            facilities: facility_groups,
            global_groups,
            advisory_issues: issues,
        },
        reference_groups,
        net_new_groups,
        similar_groups,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Facility;

    fn facility(name: &str, group: &str) -> Facility {
        Facility {
            name: name.to_string(),
            source_files: vec![format!("{name}.csv")],
            groups: HashMap::from([(group.to_string(), 1)]),
        }
    }

    #[test]
    fn a_group_recurring_across_facilities_yields_one_similarity_match_but_one_issue_per_facility()
    {
        // Neither name contains any digit/attribute token this crate's
        // fingerprinting recognizes, so both fingerprints are all-`None`
        // and trivially "match" — the only thing gating the similarity
        // score is the near-identical remainder text.
        let group_name = "Blue Sunset Bayx";
        let reference_name = "Blue Sunset Bay";

        let batch = BatchRun {
            facilities: vec![
                facility("Facility A", group_name),
                facility("Facility B", group_name),
            ],
            global_groups: HashMap::from([(group_name.to_string(), 2)]),
            advisory_issues: Vec::new(),
        };

        let results = analyze_batch(batch, Some(vec![reference_name.to_string()])).unwrap();

        assert_eq!(results.net_new_groups, vec![group_name.to_string()]);

        assert_eq!(
            results.similar_groups.len(),
            1,
            "the same net-new group name recurring across two facilities \
             should collapse into one SimilarityMatch, not one per facility"
        );

        assert_eq!(results.similar_groups[0].facility_group, group_name);
        assert_eq!(results.similar_groups[0].reference_group, reference_name);

        let similarity_issues: Vec<_> = results
            .batch_run
            .advisory_issues
            .iter()
            .filter(|issue| issue.issue.contains("Similar but not exact match"))
            .collect();

        assert_eq!(
            similarity_issues.len(),
            2,
            "each facility's own occurrence should still produce its own advisory issue"
        );

        let sources: HashSet<&str> = similarity_issues
            .iter()
            .map(|issue| issue.source.as_str())
            .collect();

        assert!(sources.contains("Facility Facility A"));
        assert!(sources.contains("Facility Facility B"));
    }

    #[test]
    fn no_reference_groups_treats_every_global_group_as_net_new() {
        let batch = BatchRun {
            facilities: vec![facility("Facility A", "10x10 Inside Climate")],
            global_groups: HashMap::from([("10x10 Inside Climate".to_string(), 1)]),
            advisory_issues: Vec::new(),
        };

        let results = analyze_batch(batch, None).unwrap();

        assert_eq!(
            results.net_new_groups,
            vec!["10x10 Inside Climate".to_string()]
        );
        assert!(results.similar_groups.is_empty());
    }
}
