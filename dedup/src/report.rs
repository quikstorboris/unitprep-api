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
        for j in (i + 1)..groups.len() {
            // Every group has at least one record — group_records never
            // creates an empty one.
            let a = groups[i].records[0].display_name();
            let b = groups[j].records[0].display_name();
            if a.is_empty() || b.is_empty() || a == b {
                continue;
            }
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
