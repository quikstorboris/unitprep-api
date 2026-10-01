//! Typo/name-variant detection — the fuzzy safety net, kept strictly
//! advisory (see crate-level docs: always flag, never auto-merge).

use std::collections::HashMap;

use crate::comparison::contact_info_matches;
use crate::note_composer::NoteComposer;
use crate::types::{TenantGroup, TenantRecord, TypoVariantCandidate};

/// Below this ratio, two names are not considered variant candidates at
/// all. Matches the reference script's `VARIANT_REVIEW_THRESHOLD`. Every
/// candidate at/above this is surfaced identically for human
/// confirmation — no separate confidence tier (deliberately dropped;
/// the reference script's `VARIANT_MERGE_THRESHOLD` distinction only
/// mattered for deciding what to auto-merge, which this crate never
/// does).
pub const VARIANT_SURFACE_THRESHOLD: f64 = 0.85;

/// Similarity between two display names: max of a straight
/// character-sequence ratio (catches spelling typos) and a
/// token-sort ratio (alphabetically sorts each name's words first,
/// catching transposed first/last names like "TED BEACH" vs
/// "BEACH TED"). Must reproduce Python's `difflib.SequenceMatcher.ratio()`
/// (Ratcliff/Obershelp) to stay compatible with the calibration set
/// already verified against real data (see project memory):
/// Zachary Cuddeback/Zachary P Cuddeback ~94%, Stephen/Stephan Tucker
/// ~92%, Ted Beach/Beach Ted 100% via token-sort, Dawn/Don Anthony ~86%,
/// Elaine/Leslie Hofstadter ~88%, Chris/Tim Neufeld ~73% (below
/// threshold).
pub fn name_similarity(a: &str, b: &str) -> f64 {
    let straight = sequence_matcher_ratio(a, b);
    let a_sorted = sort_words(a);
    let b_sorted = sort_words(b);
    let token_sort = sequence_matcher_ratio(&a_sorted, &b_sorted);
    straight.max(token_sort)
}

fn sort_words(s: &str) -> String {
    let mut words: Vec<&str> = s.split_whitespace().collect();
    words.sort_unstable();
    words.join(" ")
}

/// Ratcliff/Obershelp ratio: 2 * (total matched chars) / (len(a) + len(b)),
/// where matches are found via recursive longest-common-substring — the
/// same algorithm as Python's `difflib.SequenceMatcher.ratio()` with no
/// `isjunk`. Deliberately omits difflib's "autojunk" heuristic (which
/// only activates for sequences of 200+ elements): display names are
/// always far shorter, so it would never trigger in practice.
fn sequence_matcher_ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    let matched = total_matched(&a, &b);
    2.0 * matched as f64 / total as f64
}

/// Sums the size of every matching block found by recursively splitting
/// on the longest match, exactly matching difflib's `get_matching_blocks`
/// (minus the final block-merge pass, which doesn't change the total).
fn total_matched(a: &[char], b: &[char]) -> usize {
    let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
    for (j, &ch) in b.iter().enumerate() {
        b2j.entry(ch).or_default().push(j);
    }

    let mut total = 0;
    let mut queue = vec![(0usize, a.len(), 0usize, b.len())];
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest_match(a, &b2j, alo, ahi, blo, bhi);
        if k == 0 {
            continue;
        }
        total += k;
        if alo < i && blo < j {
            queue.push((alo, i, blo, j));
        }
        if i + k < ahi && j + k < bhi {
            queue.push((i + k, ahi, j + k, bhi));
        }
    }
    total
}

/// Longest matching run between `a[alo..ahi]` and `b[blo..bhi]`, ties
/// broken toward the earliest position in `a` then `b` — direct port of
/// difflib's `find_longest_match` dynamic-programming sweep.
fn longest_match(
    a: &[char],
    b2j: &HashMap<char, Vec<usize>>,
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> (usize, usize, usize) {
    let mut best_i = alo;
    let mut best_j = blo;
    let mut best_size = 0;

    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for (i, &ch) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut new_j2len: HashMap<usize, usize> = HashMap::new();
        if let Some(js) = b2j.get(&ch) {
            for &j in js {
                if j < blo {
                    continue;
                }
                if j >= bhi {
                    break;
                }
                let prev = if j == 0 {
                    0
                } else {
                    j2len.get(&(j - 1)).copied().unwrap_or(0)
                };
                let k = prev + 1;
                new_j2len.insert(j, k);
                if k > best_size {
                    best_i = i + 1 - k;
                    best_j = j + 1 - k;
                    best_size = k;
                }
            }
        }
        j2len = new_j2len;
    }
    (best_i, best_j, best_size)
}

/// Per-group name data computed ONCE, instead of once per pair: the
/// display name, and the sorted characters of both forms of it that
/// `name_similarity` compares (as written, and with its words sorted).
/// Sorted characters give a cheap upper bound on the similarity (below)
/// that rules out almost every pair without running the real matcher.
struct NameProfile {
    display: String,
    straight_chars: Vec<char>,
    sorted_words_chars: Vec<char>,
}

impl NameProfile {
    fn new(display: String) -> Self {
        let mut straight_chars: Vec<char> = display.chars().collect();
        straight_chars.sort_unstable();
        let mut sorted_words_chars: Vec<char> = sort_words(&display).chars().collect();
        sorted_words_chars.sort_unstable();
        Self {
            display,
            straight_chars,
            sorted_words_chars,
        }
    }
}

/// Size of the multiset intersection of two sorted character slices.
fn multiset_intersection(a: &[char], b: &[char]) -> usize {
    let (mut i, mut j, mut common) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                common += 1;
                i += 1;
                j += 1;
            }
        }
    }
    common
}

/// The most `sequence_matcher_ratio` could possibly be for two strings
/// with these sorted characters. Every matched block pairs equal
/// characters one-to-one, so the matched total can never exceed the
/// multiset intersection; the ratio is monotonic in the matched total,
/// so this bound is never below the true ratio. Pairs whose bound is
/// under the surface threshold are skipped with no change to the result.
fn ratio_upper_bound(a: &[char], b: &[char]) -> f64 {
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    2.0 * multiset_intersection(a, b) as f64 / total as f64
}

fn could_reach_threshold(a: &NameProfile, b: &NameProfile) -> bool {
    let straight = ratio_upper_bound(&a.straight_chars, &b.straight_chars);
    let sorted = ratio_upper_bound(&a.sorted_words_chars, &b.sorted_words_chars);
    straight.max(sorted) >= VARIANT_SURFACE_THRESHOLD
}

/// Pass over every pair of distinct-key groups, surfacing any whose
/// display names are similar enough to be the same tenant under a
/// typo/variant spelling. Unlike the reference script's
/// `classify_variant_pairs`, this never merges groups or writes a
/// combined row into anything — every candidate above threshold is
/// returned as-is for a human to confirm (see crate-level docs).
///
/// Quadratic in the number of groups, so each name is prepared once and
/// every pair first passes a cheap, exact upper-bound check
/// (`could_reach_threshold`) before the real matcher runs. On a
/// 759-row facility that cut ~8.5 s (debug build) to well under a
/// second without changing a single result.
pub fn find_typo_variant_candidates(
    groups: &[TenantGroup],
    composer: &dyn NoteComposer,
) -> Vec<TypoVariantCandidate> {
    // Every group has at least one record — group_records never
    // creates an empty one. A blank display name has nothing to compare.
    let profiles: Vec<Option<NameProfile>> = groups
        .iter()
        .map(|group| {
            let display = group.records[0].display_name();
            (!display.is_empty()).then(|| NameProfile::new(display))
        })
        .collect();

    let mut candidates = Vec::new();
    for i in 0..groups.len() {
        let Some(a) = &profiles[i] else {
            continue;
        };
        for j in (i + 1)..groups.len() {
            let Some(b) = &profiles[j] else {
                continue;
            };
            // NOTE: two *different* group keys (distinct `FirtLast`
            // spellings/formatting) can still produce an identical
            // display name — e.g. "Smith, John" vs. "John  Smith" both
            // display as "John Smith" but never collapse into one
            // `group_key`. That's exactly the strongest duplicate-tenant
            // signal there is, so it must NOT be skipped here; a 100%
            // `name_similarity` ratio surfaces it through the normal
            // threshold check below like any other high-similarity pair.
            if !could_reach_threshold(a, b) {
                continue;
            }
            let ratio = name_similarity(&a.display, &b.display);
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
    // `partial_cmp(...).unwrap()` would panic on a NaN ratio. Nothing
    // produces one today (blank display names are filtered out above,
    // and `sequence_matcher_ratio` special-cases zero-length input to
    // return 1.0 rather than dividing 0/0) -- `total_cmp` costs nothing
    // over `partial_cmp` in the common case and removes the panic path
    // entirely rather than relying on that invariant holding forever.
    candidates.sort_by(|a, b| b.ratio.total_cmp(&a.ratio));
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note_composer::TemplateNoteComposer;

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

    /// Every value here was computed by actually running Python's
    /// `difflib.SequenceMatcher` (the reference implementation) on the
    /// same pairs, not estimated — see project memory's calibration set
    /// for where these names came from (real production data).
    fn assert_ratio(a: &str, b: &str, expected_percent: f64) {
        let ratio = name_similarity(a, b) * 100.0;
        assert!(
            (ratio - expected_percent).abs() < 0.01,
            "name_similarity({a:?}, {b:?}) = {ratio:.2}%, expected {expected_percent:.2}%"
        );
    }

    #[test]
    fn zachary_cuddeback_variant_merges() {
        assert_ratio("ZACHARY CUDDEBACK", "ZACHARY P CUDDEBACK", 94.44);
    }

    #[test]
    fn stephen_stephan_tucker_merges() {
        assert_ratio("STEPHEN TUCKER", "STEPHAN TUCKER", 92.86);
    }

    #[test]
    fn transposed_name_catches_via_token_sort() {
        assert_ratio("TED BEACH", "BEACH TED", 100.0);
    }

    #[test]
    fn dawn_don_anthony_is_tier_two() {
        assert_ratio("DAWN ANTHONY", "DON ANTHONY", 86.96);
    }

    #[test]
    fn hofstadter_sisters_is_tier_two() {
        assert_ratio("ELAINE HOFSTADTER", "LESLIE HOFSTADTER", 88.24);
    }

    #[test]
    fn unrelated_names_fall_below_surface_threshold() {
        let ratio = name_similarity("CHRIS NEUFELD", "TIM NEUFELD");
        assert!(
            ratio < VARIANT_SURFACE_THRESHOLD,
            "expected below threshold, got {ratio}"
        );
    }

    /// The reference behavior the pruning must reproduce exactly: the
    /// original unpruned loop, comparing every pair with the real matcher.
    fn brute_force_pairs(names: &[String]) -> Vec<(usize, usize)> {
        let mut pairs = Vec::new();
        for i in 0..names.len() {
            for j in (i + 1)..names.len() {
                if names[i].is_empty() || names[j].is_empty() {
                    continue;
                }
                if name_similarity(&names[i], &names[j]) >= VARIANT_SURFACE_THRESHOLD {
                    pairs.push((i, j));
                }
            }
        }
        pairs
    }

    /// A deterministic set of names with every kind of near-duplicate the
    /// threshold exists for: typos, transposed words, middle initials,
    /// doubled spaces, case, punctuation, short names, and many unrelated
    /// ones.
    fn sample_names() -> Vec<String> {
        let firsts = [
            "John", "Jon", "Jane", "Zachary", "Stephen", "Stephan", "Dawn", "Don", "Elaine",
            "Leslie", "Ted", "Chris", "Tim", "Ana", "Li", "Maria", "Marie", "Robert", "Roberto",
            "Kim",
        ];
        let lasts = [
            "Smith",
            "Smyth",
            "Tucker",
            "Anthony",
            "Beach",
            "Neufeld",
            "Hofstadter",
            "Cuddeback",
            "Ng",
            "Lee",
            "Flores",
            "Floras",
            "Garcia",
            "Garcias",
            "O'Brien",
            "Obrien",
        ];
        let mut names = Vec::new();
        let mut state = 12345u64;
        let mut next = |bound: usize| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as usize) % bound
        };
        for _ in 0..140 {
            let first = firsts[next(firsts.len())];
            let last = lasts[next(lasts.len())];
            names.push(match next(6) {
                0 => format!("{first} {last}"),
                1 => format!("{last} {first}"),
                2 => format!("{first} P {last}"),
                3 => format!("{first}  {last}"),
                4 => format!("{} {last}", first.to_uppercase()),
                _ => format!("{first} {last}"),
            });
        }
        names.push(String::new());
        names.push("A".to_string());
        names
    }

    #[test]
    fn pruning_returns_exactly_the_pairs_the_unpruned_loop_would() {
        let names = sample_names();
        let groups: Vec<TenantGroup> = names
            .iter()
            .enumerate()
            .map(|(i, name)| TenantGroup {
                key: format!("k{i}"),
                records: vec![TenantRecord {
                    first_last: name.clone(),
                    ..Default::default()
                }],
            })
            .collect();
        // display_name() title-cases first_last when first/last are blank;
        // compute the same strings the pass compares.
        let displays: Vec<String> = groups.iter().map(|g| g.records[0].display_name()).collect();

        let expected = brute_force_pairs(&displays);
        let mut actual: Vec<(usize, usize)> =
            find_typo_variant_candidates(&groups, &crate::note_composer::TemplateNoteComposer)
                .iter()
                .map(|c| {
                    let idx = |key: &str| key[1..].parse::<usize>().unwrap();
                    (idx(&c.key_a), idx(&c.key_b))
                })
                .collect();
        actual.sort_unstable();

        assert!(
            !expected.is_empty(),
            "the sample must contain real candidates"
        );
        assert_eq!(actual, expected);
    }

    #[test]
    fn the_upper_bound_is_never_below_the_true_similarity() {
        let names: Vec<String> = sample_names()
            .into_iter()
            .filter(|n| !n.is_empty())
            .collect();
        for a in &names {
            for b in &names {
                let (pa, pb) = (NameProfile::new(a.clone()), NameProfile::new(b.clone()));
                let bound = ratio_upper_bound(&pa.straight_chars, &pb.straight_chars).max(
                    ratio_upper_bound(&pa.sorted_words_chars, &pb.sorted_words_chars),
                );
                assert!(
                    bound >= name_similarity(a, b),
                    "bound {bound} below true ratio for {a:?} vs {b:?}"
                );
            }
        }
    }
}
