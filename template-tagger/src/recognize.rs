use crate::detect::{find_word_bounded_matches, Candidate};

/// One authored `label_proximity` row from `client_ops.tag_pattern`,
/// loaded by the caller -- this crate has no DB access of its own,
/// matching [`crate::detect_candidates`]'s own "pure text matching, no
/// I/O" scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelProximityPattern {
    pub tag_key: String,
    pub label: String,
    pub position: LabelPosition,
    pub max_gap_chars: usize,
    /// A second label that must appear somewhere in the
    /// `within_chars` immediately *before* this pattern's own label,
    /// or the match is skipped entirely. Exists for the common
    /// "the same field label is reused for two different people/
    /// sections" shape -- a document with both an occupant and an
    /// alternate contact often labels both address blanks bare
    /// `ADDRESS:`, with nothing to tell them apart except which
    /// section header (`OCCUPANT NAME` vs. `ALTERNATE NAME`) came
    /// before each one. `None` for the common case of a label that
    /// only ever appears once.
    pub requires_preceding_anchor: Option<PrecedingAnchor>,
}

/// See [`LabelProximityPattern::requires_preceding_anchor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrecedingAnchor {
    pub text: String,
    pub within_chars: usize,
}

/// Where the blank sits relative to the pattern's label -- `After` for
/// `"Move-In Date: ___"` (blank follows the label), `Before` for
/// `"___ (Tenant Signature)"` (blank precedes it).
///
/// A label embedded literally *inside* a blank run (e.g. Budget Self
/// Storage's `"$____Total Amount______"`) is a real case found in the
/// corpus but not yet implemented -- deliberately not a variant here,
/// so a pattern authored for that shape has nothing to match against
/// yet rather than silently matching the wrong thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelPosition {
    Before,
    After,
}

/// A run shorter than this is not treated as a blank -- a single `_` or
/// `__` is more likely a stray character (a snake_case value, a typo)
/// than a fill-in field. Three was picked to match the shortest blank
/// actually seen written by hand in the corpus (`___`); it is not a
/// value tuned against real false positives, since none have been found
/// yet -- revisit if one turns up.
const MIN_BLANK_LEN: usize = 3;

/// Finds every literal underscore-run blank in `document_text` that sits
/// within `max_gap_chars` characters of one of `patterns`' labels, on
/// the side the pattern specifies, and proposes filling that blank with
/// the pattern's `tag_key`.
///
/// Reuses [`crate::detect_candidates`]'s own ASCII-only, word-bounded
/// label matching -- same ASCII-case-folding tradeoff, same rationale.
///
/// Only catches underscore-character blanks (`___`). A blank
/// represented purely through Word's underline character formatting,
/// with zero literal `_` bytes, is invisible to plain-text matching --
/// that is a `.docx`-structural signal `docx-surgeon`'s text extraction
/// would need to surface as its own marker. Out of scope here by
/// construction (this crate never sees anything but flattened text),
/// not an oversight -- see the corpus finding that motivated this note
/// (Northwest Heated Mini Storage: 192 underlined blanks, zero literal
/// underscores).
///
/// - **A blank can match more than one pattern.** Two different tags'
///   labels can each sit close enough to the same blank (e.g. a generic
///   "Date" label near several date-shaped blanks in a table); every
///   real match is reported independently, same
///   never-deduplicate-here philosophy as `detect_candidates`.
/// - **Does not require the closest label to win.** `max_gap_chars` is
///   only a cutoff, not a ranking signal -- picking the single best
///   label for an ambiguous blank is a judgment call for pattern
///   authoring (narrower `max_gap_chars`, more specific labels) or the
///   human reviewer, not something this pure matcher decides silently.
pub fn recognize_blanks(document_text: &str, patterns: &[LabelProximityPattern]) -> Vec<Candidate> {
    let blanks = find_blank_runs(document_text);
    let mut candidates = Vec::new();

    for pattern in patterns {
        for (label_start, label_end) in find_word_bounded_matches(document_text, &pattern.label) {
            if let Some(anchor) = &pattern.requires_preceding_anchor {
                if !anchor_precedes(document_text, label_start, anchor) {
                    continue;
                }
            }

            for &(blank_start, blank_end) in &blanks {
                let gap = match pattern.position {
                    LabelPosition::After if blank_start >= label_end => blank_start - label_end,
                    LabelPosition::Before if label_start >= blank_end => label_start - blank_end,
                    _ => continue,
                };

                if gap <= pattern.max_gap_chars {
                    candidates.push(Candidate {
                        tag_key: pattern.tag_key.clone(),
                        matched_text: document_text[blank_start..blank_end].to_string(),
                        start: blank_start,
                        end: blank_end,
                    });
                }
            }
        }
    }

    candidates.sort_by_key(|candidate| candidate.start);
    candidates
}

/// Whether `anchor.text` appears (word-bounded, same ASCII-folding
/// convention as every other match in this crate) anywhere in the
/// `anchor.within_chars` bytes immediately before `label_start`. Only
/// looks backward -- an anchor is a *preceding* section header, not
/// just "somewhere nearby" -- so a document with two sections sharing
/// a label never lets the later section's anchor satisfy the earlier
/// section's pattern (position 750 is never "before" position 200).
fn anchor_precedes(document_text: &str, label_start: usize, anchor: &PrecedingAnchor) -> bool {
    let window_start = char_boundary_at_or_after(
        document_text,
        label_start.saturating_sub(anchor.within_chars),
    );
    let window = &document_text[window_start..label_start];
    !find_word_bounded_matches(window, &anchor.text).is_empty()
}

/// The smallest char-boundary-safe index `>= idx` -- `label_start -
/// within_chars` is an arbitrary byte offset that can land mid-
/// character if the text contains anything outside ASCII; walking
/// forward to the next real boundary keeps the slice in
/// [`anchor_precedes`] from panicking.
fn char_boundary_at_or_after(text: &str, mut idx: usize) -> usize {
    while idx < text.len() && !text.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// Every byte-range `(start, end)` in `text` covering a maximal run of
/// `_` characters at least [`MIN_BLANK_LEN`] long.
fn find_blank_runs(text: &str) -> Vec<(usize, usize)> {
    let mut runs = Vec::new();
    let mut run_start: Option<usize> = None;

    for (i, c) in text.char_indices() {
        if c == '_' {
            run_start.get_or_insert(i);
        } else if let Some(start) = run_start.take() {
            push_if_long_enough(&mut runs, start, i);
        }
    }
    if let Some(start) = run_start {
        push_if_long_enough(&mut runs, start, text.len());
    }

    runs
}

fn push_if_long_enough(runs: &mut Vec<(usize, usize)>, start: usize, end: usize) {
    if end - start >= MIN_BLANK_LEN {
        runs.push((start, end));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(
        tag_key: &str,
        label: &str,
        position: LabelPosition,
        max_gap_chars: usize,
    ) -> LabelProximityPattern {
        LabelProximityPattern {
            tag_key: tag_key.to_string(),
            label: label.to_string(),
            position,
            max_gap_chars,
            requires_preceding_anchor: None,
        }
    }

    fn anchored_pattern(
        tag_key: &str,
        label: &str,
        anchor_text: &str,
        within_chars: usize,
    ) -> LabelProximityPattern {
        LabelProximityPattern {
            requires_preceding_anchor: Some(PrecedingAnchor {
                text: anchor_text.to_string(),
                within_chars,
            }),
            ..pattern(tag_key, label, LabelPosition::After, 5)
        }
    }

    #[test]
    fn finds_a_blank_after_its_label() {
        let text = "Move-In Date: ___________";
        let candidates = recognize_blanks(
            text,
            &[pattern("m.indate", "Move-In Date", LabelPosition::After, 5)],
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].tag_key, "m.indate");
        assert_eq!(candidates[0].matched_text, "___________");
    }

    #[test]
    fn finds_a_blank_before_its_label() {
        let text = "___________ (Tenant Signature)";
        let candidates = recognize_blanks(
            text,
            &[pattern(
                "e.name",
                "Tenant Signature",
                LabelPosition::Before,
                5,
            )],
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].tag_key, "e.name");
    }

    #[test]
    fn respects_the_max_gap_cutoff() {
        let text = "Move-In Date: this sentence is way too long to be a real gap _____";
        let candidates = recognize_blanks(
            text,
            &[pattern("m.indate", "Move-In Date", LabelPosition::After, 5)],
        );

        assert!(candidates.is_empty());
    }

    #[test]
    fn ignores_underscore_runs_shorter_than_the_minimum() {
        let text = "Move-In Date: __";
        let candidates = recognize_blanks(
            text,
            &[pattern("m.indate", "Move-In Date", LabelPosition::After, 5)],
        );

        assert!(candidates.is_empty());
    }

    #[test]
    fn is_case_insensitive_for_ascii_labels() {
        let text = "MOVE-IN DATE: ___________";
        let candidates = recognize_blanks(
            text,
            &[pattern("m.indate", "Move-In Date", LabelPosition::After, 5)],
        );

        assert_eq!(candidates.len(), 1);
    }

    #[test]
    fn reports_every_pattern_that_matches_the_same_blank() {
        // A deliberately ambiguous case: two labels close enough to the
        // same blank both get reported, undeduplicated -- resolving the
        // ambiguity is out of scope for this pure matcher.
        let text = "Date: ___________";
        let candidates = recognize_blanks(
            text,
            &[
                pattern("m.indate", "Date", LabelPosition::After, 5),
                pattern("l.ptd", "Date", LabelPosition::After, 5),
            ],
        );

        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().any(|c| c.tag_key == "m.indate"));
        assert!(candidates.iter().any(|c| c.tag_key == "l.ptd"));
    }

    #[test]
    fn reports_no_candidate_when_the_label_never_appears() {
        let text = "This document never mentions a move-in date at all: ___________";
        let candidates = recognize_blanks(
            text,
            &[pattern("m.indate", "Move-In Date", LabelPosition::After, 5)],
        );

        assert!(candidates.is_empty());
    }

    #[test]
    fn reports_no_candidate_when_no_blank_is_nearby() {
        let text = "Move-In Date: February 3rd, 2026";
        let candidates = recognize_blanks(
            text,
            &[pattern("m.indate", "Move-In Date", LabelPosition::After, 5)],
        );

        assert!(candidates.is_empty());
    }

    #[test]
    fn an_anchored_pattern_only_matches_the_occurrence_after_its_anchor() {
        // The exact real-world shape this exists for: the same bare
        // label ("ADDRESS:") reused for two different people, each
        // introduced by its own section header.
        let text = "OCCUPANT NAME: John\nADDRESS: ________\n\n\
                     ALTERNATE NAME: Jane\nADDRESS: ________";
        let occupant_address = anchored_pattern("e.address", "ADDRESS:", "OCCUPANT NAME", 30);

        let candidates = recognize_blanks(text, &[occupant_address]);

        assert_eq!(candidates.len(), 1);
        // The first ADDRESS: (right after OCCUPANT NAME), not the second.
        assert_eq!(candidates[0].start, text.find("________").unwrap());
    }

    #[test]
    fn two_anchored_patterns_correctly_split_a_document_with_two_sections() {
        let text = "OCCUPANT NAME: John\nADDRESS: ________\n\n\
                     ALTERNATE NAME: Jane\nADDRESS: ________";
        let occupant_address = anchored_pattern("e.address", "ADDRESS:", "OCCUPANT NAME", 30);
        let alternate_address = anchored_pattern("e.a.address", "ADDRESS:", "ALTERNATE NAME", 30);

        let candidates = recognize_blanks(text, &[occupant_address, alternate_address]);

        // Two candidates, not four -- each pattern matched exactly the
        // one occurrence its anchor actually precedes, so neither blank
        // ends up ambiguous (tier NeedsReview) the way an unanchored
        // pair sharing the same bare label would.
        assert_eq!(candidates.len(), 2);
        let first_blank = text.find("________").unwrap();
        let second_blank = text.rfind("________").unwrap();
        assert!(candidates
            .iter()
            .any(|c| c.tag_key == "e.address" && c.start == first_blank));
        assert!(candidates
            .iter()
            .any(|c| c.tag_key == "e.a.address" && c.start == second_blank));
    }

    #[test]
    fn an_anchor_further_back_than_within_chars_does_not_satisfy_the_pattern() {
        let text = "OCCUPANT NAME: John and a lot of padding text in between here\
                     ADDRESS: ________";
        let occupant_address = anchored_pattern("e.address", "ADDRESS:", "OCCUPANT NAME", 10);

        let candidates = recognize_blanks(text, &[occupant_address]);

        assert!(candidates.is_empty());
    }
}
