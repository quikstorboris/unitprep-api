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
    parse_fingerprint,
    Climate,
    GroupFingerprint,
    Location,
};
pub use reference::{
    load_reference_groups_from_document,
    select_group_document,
};

use std::collections::HashSet;

use anyhow::Result;
use strsim::normalized_levenshtein;

use fingerprint::fingerprints_match;

use crate::domain::models::{
    AnalysisResults,
    BatchRun,
    Issue,
    Severity,
    SimilarityMatch,
};

pub fn analyze_batch(
    batch: BatchRun,
    reference_groups: Option<Vec<String>>,
) -> Result<AnalysisResults> {
    let mut issues =
        batch
            .advisory_issues
            .clone();

    let global_groups =
        batch
            .global_groups
            .clone();

    let mut facility_groups =
        batch
            .facilities
            .clone();

    let mut net_new_groups =
        Vec::new();

    let mut similar_groups =
        Vec::new();

    if let Some(reference_groups) =
        &reference_groups
    {
        let reference_set:
            HashSet<_> =
            reference_groups
                .iter()
                .cloned()
                .collect();

        let parsed_reference_groups =
            reference_groups
                .iter()
                .map(|group| {
                    (
                        group.clone(),
                        parse_fingerprint(
                            group,
                        ),
                    )
                })
                .collect::<Vec<_>>();

        for facility in
            &mut facility_groups
        {
            for group in
                facility.groups.keys()
            {
                if reference_set
                    .contains(group)
                {
                    continue;
                }

                net_new_groups.push(
                    group.clone(),
                );

                let parsed_group =
                    parse_fingerprint(
                        group,
                    );

                let mut best_match =
                    None;

                let mut best_score =
                    0.0_f64;

                for (
                    candidate,
                    candidate_fp,
                ) in &parsed_reference_groups
                {
                    if !fingerprints_match(
                        &parsed_group,
                        candidate_fp,
                    ) {
                        continue;
                    }

                    let score =
                        normalized_levenshtein(
                            &parsed_group
                                .remainder,
                            &candidate_fp
                                .remainder,
                        );

                    if score
                        > best_score
                    {
                        best_score =
                            score;

                        best_match =
                            Some(
                                candidate
                                    .clone(),
                            );
                    }
                }

                if let Some(
                    candidate,
                ) = best_match
                {
                    if best_score
                        >= 0.80
                        && candidate
                            != *group
                    {
                        similar_groups
                            .push(
                                SimilarityMatch {
                                    facility_group:
                                        group
                                            .clone(),
                                    reference_group:
                                        candidate
                                            .clone(),
                                    similarity:
                                        best_score,
                                    difference:
                                        format!(
                                            "{} -> {}",
                                            group,
                                            candidate
                                        ),
                                },
                            );

                        issues.push(
                            Issue {
                                source:
                                    format!(
                                        "Facility {}",
                                        facility
                                            .name
                                    ),
                                issue:
                                    format!(
                                        "Similar but not exact match found: '{}' vs '{}' (score {:.2})",
                                        group,
                                        candidate,
                                        best_score
                                    ),
                                severity:
                                    Severity::Warning,
                            },
                        );
                    }
                }
            }
        }
    } else {
        net_new_groups.extend(
            global_groups
                .keys()
                .cloned(),
        );
    }

    net_new_groups.sort();
    net_new_groups.dedup();

    similar_groups.sort_by(
        |a, b| {
            b.similarity
                .partial_cmp(
                    &a.similarity,
                )
                .unwrap_or(
                    std::cmp::Ordering::Equal,
                )
        },
    );

    Ok(AnalysisResults {
        batch_run:
            BatchRun {
                facilities:
                    facility_groups,
                global_groups,
                advisory_issues:
                    issues,
            },
        reference_groups:
            reference_groups
                .clone(),
        net_new_groups,
        similar_groups,
    })
}