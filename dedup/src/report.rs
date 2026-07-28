//! Orchestrates the three passes (group → compare → note) plus the
//! typo-variant safety net into one `DedupReport`. Ported from the
//! reference script's `main()`, minus all file I/O and CLI/summary
//! printing — this crate returns structured data; presentation is an
//! API/UI-layer concern.

use serde::Serialize;

use crate::comparison::{contact_info_matches, find_differing_categories};
use crate::grouping::{group_records, multi_unit_groups};
use crate::note_composer::{NoteComposer, TemplateNoteComposer};
use crate::relatedness::{find_related_tenant_candidates, RelatedTenantCandidate};
use crate::similarity::{name_similarity, VARIANT_SURFACE_THRESHOLD};
use crate::types::{FlaggedGroup, TenantGroup, TenantRecord, TypoVariantCandidate};

/// Full result of a duplicate-tenant check run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DedupReport {
    pub total_rows: usize,
    pub unique_tenants: usize,
    pub multi_unit_tenants: usize,
    pub flagged_groups: Vec<FlaggedGroup>,
    pub typo_variant_candidates: Vec<TypoVariantCandidate>,
    pub related_tenant_candidates: Vec<RelatedTenantCandidate>,
}

/// Runs the full duplicate-tenant check over `records`, composing notes
/// with the default `TemplateNoteComposer`. See `run_with_composer` to
/// supply a different one (e.g. a future AI-backed composer).
pub fn run(records: Vec<TenantRecord>) -> DedupReport {
    run_with_composer(records, &TemplateNoteComposer)
}

/// Same as `run`, with an explicit `NoteComposer` — the seam for
/// swapping how note text gets written without touching any of the
/// matching/comparison logic above it.
pub fn run_with_composer(records: Vec<TenantRecord>, composer: &dyn NoteComposer) -> DedupReport {
    let total_rows = records.len();
    let groups = group_records(records);
    let unique_tenants = groups.len();

    // Typo-variant candidates and related-tenant candidates are both
    // found across *every* tenant, including single-unit ones — a
    // relationship or a typo/variant can exist between two single-unit
    // tenants just as easily as multi-unit ones. Matches the reference
    // script's own typo-variant pass, which runs over the full groups
    // dict, not just the multi-unit subset. Both must happen before
    // `multi_unit_groups` consumes `groups`.
    let typo_variant_candidates = find_typo_variant_candidates(&groups, composer);
    let related_tenant_candidates = find_related_tenant_candidates(&groups, composer);

    let multi = multi_unit_groups(groups);
    let multi_unit_tenants = multi.len();

    let flagged_groups = flag_groups(multi, composer);

    DedupReport {
        total_rows,
        unique_tenants,
        multi_unit_tenants,
        flagged_groups,
        typo_variant_candidates,
        related_tenant_candidates,
    }
}

fn flag_groups(groups: Vec<TenantGroup>, composer: &dyn NoteComposer) -> Vec<FlaggedGroup> {
    groups
        .into_iter()
        .filter_map(|group| {
            let differing = find_differing_categories(&group.records);
            if differing.is_empty() {
                return None;
            }
            let note = composer.compose_group_note(&group, &differing);
            Some(FlaggedGroup {
                group,
                mismatches: differing,
                note,
            })
        })
        .collect()
}

/// Pass over every pair of distinct-key groups, surfacing any whose
/// display names are similar enough to be the same tenant under a
/// typo/variant spelling. Unlike the reference script's
/// `classify_variant_pairs`, this never merges groups or writes a
/// combined row into anything — every candidate above threshold is
/// returned as-is for a human to confirm (see crate-level docs).
fn find_typo_variant_candidates(
    groups: &[TenantGroup],
    composer: &dyn NoteComposer,
) -> Vec<TypoVariantCandidate> {
    let mut candidates = Vec::new();
    for i in 0..groups.len() {
        // Every group has at least one record — group_records never
        // creates an empty one. `a` depends only on `i`, so compute it
        // once per outer iteration instead of once per (i, j) pair — the
        // inner loop can run many times per `i` in a large facility.
        let a = groups[i].records[0].display_name();
        if a.is_empty() {
            continue;
        }
        for j in (i + 1)..groups.len() {
            let b = groups[j].records[0].display_name();
            if b.is_empty() {
                continue;
            }
            // NOTE: two *different* group keys (distinct `FirtLast`
            // spellings/formatting) can still produce an identical
            // display name — e.g. "Smith, John" vs. "John  Smith" both
            // display as "John Smith" but never collapse into one
            // `group_key`. That's exactly the strongest duplicate-tenant
            // signal there is, so it must NOT be skipped here; a 100%
            // `name_similarity` ratio surfaces it through the normal
            // threshold check below like any other high-similarity pair.
            let ratio = name_similarity(&a, &b);
            if ratio < VARIANT_SURFACE_THRESHOLD {
                continue;
            }
            let combined: Vec<TenantRecord> = groups[i]
                .records
                .iter()
                .chain(groups[j].records.iter())
                .cloned()
                .collect();
            let matches = contact_info_matches(&combined);
            candidates.push(TypoVariantCandidate {
                key_a: groups[i].key.clone(),
                key_b: groups[j].key.clone(),
                ratio,
                contact_info_matches: matches,
                note: composer.compose_variant_note(&groups[i], &groups[j], matches),
            });
        }
    }
    candidates.sort_by(|a, b| b.ratio.partial_cmp(&a.ratio).unwrap());
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TenantRecord;

    fn record(first_last: &str, first_name: &str, last_name: &str, unit: &str) -> TenantRecord {
        TenantRecord {
            first_last: first_last.to_string(),
            first_name: first_name.to_string(),
            last_name: last_name.to_string(),
            unit_number: unit.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn identical_display_names_under_different_keys_are_surfaced() {
        // Two different `FirtLast` spellings/formats ("Smith, John" vs.
        // "John  Smith") never share a group_key, but both display as
        // "John Smith" — exactly the strongest duplicate-tenant signal
        // there is. This must NOT be silently skipped.
        let groups = vec![
            TenantGroup {
                key: "smith, john".to_string(),
                records: vec![record("Smith, John", "John", "Smith", "A1")],
            },
            TenantGroup {
                key: "john  smith".to_string(),
                records: vec![record("John  Smith", "John", "Smith", "B2")],
            },
        ];

        let candidates = find_typo_variant_candidates(&groups, &TemplateNoteComposer);

        assert_eq!(candidates.len(), 1);
        assert!(
            (candidates[0].ratio - 1.0).abs() < 1e-9,
            "expected a perfect-match ratio, got {}",
            candidates[0].ratio
        );
    }

    #[test]
    fn blank_display_names_are_still_skipped() {
        let groups = vec![
            TenantGroup {
                key: "a".to_string(),
                records: vec![record("", "", "", "A1")],
            },
            TenantGroup {
                key: "b".to_string(),
                records: vec![record("", "", "", "B2")],
            },
        ];

        let candidates = find_typo_variant_candidates(&groups, &TemplateNoteComposer);

        assert!(candidates.is_empty());
    }

    /// End-to-end, fabricated (no real PII) fixture exercising all three
    /// passes together in one `run()` call — grouping, flagging,
    /// typo-variant, and relatedness all interacting on the same
    /// dataset. The only other full-pipeline tests in this crate are the
    /// two `#[ignore]`d real-data fixtures in `tests/reference_fixtures.rs`,
    /// which need real PII files on disk and don't run in CI — this one
    /// runs every time and would catch a regression where fixing one
    /// pass breaks an assumption another pass depends on.
    #[test]
    fn full_pipeline_runs_all_three_passes_together_on_fabricated_data() {
        fn r(
            first_last: &str,
            first_name: &str,
            last_name: &str,
            unit: &str,
            email: &str,
            phone: &str,
        ) -> TenantRecord {
            TenantRecord {
                first_last: first_last.to_string(),
                first_name: first_name.to_string(),
                last_name: last_name.to_string(),
                unit_number: unit.to_string(),
                email: email.to_string(),
                phone_number: phone.to_string(),
                ..Default::default()
            }
        }

        let records = vec![
            // "Smith, John" — two units, same tenant, one has an email
            // on file and the other doesn't. A real contact-info
            // mismatch: should surface in `flagged_groups`.
            r(
                "Smith, John",
                "John",
                "Smith",
                "A1",
                "john@example.com",
                "5551110001",
            ),
            r("Smith, John", "John", "Smith", "A2", "", "5551110001"),
            // "John Smith" — a different raw `FirtLast` key from the
            // group above, but the same display name once titled. The
            // strongest duplicate-tenant signal there is: should surface
            // as a typo-variant candidate against "Smith, John" above.
            r(
                "John Smith",
                "John",
                "Smith",
                "B1",
                "someone-else@example.com",
                "5551110099",
            ),
            // "Maria Garcia" and "Robert Chen" — unrelated tenants by
            // name, but sharing a phone number nobody else has: should
            // surface as a related-tenant candidate.
            r(
                "Maria Garcia",
                "Maria",
                "Garcia",
                "C1",
                "maria@example.com",
                "5559876543",
            ),
            r(
                "Robert Chen",
                "Robert",
                "Chen",
                "D1",
                "robert@example.com",
                "5559876543",
            ),
        ];

        let report = run(records);

        assert_eq!(report.total_rows, 5);
        assert_eq!(report.unique_tenants, 4, "4 distinct FirtLast keys");
        assert_eq!(
            report.multi_unit_tenants, 1,
            "only \"Smith, John\" has 2+ units"
        );

        assert_eq!(report.flagged_groups.len(), 1);
        assert!(
            report.flagged_groups[0]
                .mismatches
                .iter()
                .any(|m| m.category == crate::types::FieldCategory::Email),
            "the flagged group should be the email mismatch between A1 and A2"
        );

        assert_eq!(report.typo_variant_candidates.len(), 1);
        let variant = &report.typo_variant_candidates[0];
        assert!(
            (variant.ratio - 1.0).abs() < 1e-9,
            "\"Smith, John\" and \"John Smith\" should be a perfect display-name match"
        );

        assert_eq!(report.related_tenant_candidates.len(), 1);
        let related = &report.related_tenant_candidates[0];
        assert_eq!(
            related.signal,
            crate::relatedness::RelatednessSignal::SharedPhone
        );
        assert_eq!(related.shared_value, "5559876543");
        assert_eq!(
            related.group_keys,
            vec!["maria garcia".to_string(), "robert chen".to_string()]
        );
    }
}
