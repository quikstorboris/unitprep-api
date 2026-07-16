//! Typo/name-variant detection — the fuzzy safety net, kept strictly
//! advisory (see crate-level docs: always flag, never auto-merge).

use std::collections::HashMap;

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
                let prev = if j == 0 { 0 } else { j2len.get(&(j - 1)).copied().unwrap_or(0) };
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(ratio < VARIANT_SURFACE_THRESHOLD, "expected below threshold, got {ratio}");
    }
}
