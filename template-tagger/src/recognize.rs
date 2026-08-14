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

/// A run shorter than this is not treated as a plausible filled value --
/// same reasoning as [`MIN_BLANK_LEN`], picked to rule out a label
/// matching immediately against stray punctuation with nothing real
/// after it (e.g. two adjacent labels sharing one line with nothing
/// between them). Unlike a blank, a legitimate filled value can
/// meaningfully be very short (a one-digit unit number), so this is
/// deliberately smaller than `MIN_BLANK_LEN`.
const MIN_FILLED_VALUE_LEN: usize = 1;

/// A same-line connector between a label and its value -- a tab, a
/// plain space, a colon, or `#`. Deliberately excludes `\n`: a label
/// immediately followed by a paragraph break with nothing else on its
/// line means "no value here," not "skip the break and treat the next
/// paragraph as this label's value." Used to skip past the label's own
/// connective punctuation before searching for where the value ends.
fn is_value_connector(c: char) -> bool {
    (c.is_whitespace() && c != '\n') || c == ':' || c == '#'
}

/// Trailing punctuation trimmed off the far end of a filled-value span
/// -- sentence punctuation (`.`, `,`) and same-line whitespace that
/// belongs to the surrounding prose, not the value itself. Also
/// excludes `\n` for the same reason as [`is_value_connector`], though
/// in practice the boundary search never lets a `\n` reach this trim in
/// the first place (see `value_span_after`/`value_span_before`, which
/// cut the span's end/start exactly at a `\n`'s own position).
fn is_trailing_punctuation(c: char) -> bool {
    (c.is_whitespace() && c != '\n') || c == '.' || c == ','
}

/// Same job as [`recognize_blanks`], but for a label's value when it's
/// already filled in with real text rather than left blank -- a
/// system-generated notice or a completed lease still has the same
/// "label sits next to its value" shape a blank template does, it's
/// just that the value is real content instead of an underscore run.
///
/// Since a filled value has no unambiguous self-delimiting marker the
/// way a run of `_` does, its span is bounded by the nearest of:
/// another matched label's position, a paragraph break (`\n` --
/// `docx_surgeon::read_docx` inserts one between paragraphs within a
/// region), a tab character (a column separator on the same line,
/// e.g. `"FROM:\tNo Ka Oi Self Storage"`), or `max_gap_chars` itself.
/// Whichever comes first wins; the match is then trimmed of leading
/// connector punctuation ([`is_value_connector`]) and trailing sentence
/// punctuation ([`is_trailing_punctuation`]).
///
/// Deliberately conservative in two ways that keep this from ever
/// silently competing with [`recognize_blanks`] on the same span: a
/// trimmed span that ends up empty, shorter than [`MIN_FILLED_VALUE_LEN`],
/// or consisting entirely of `_` (an actual blank -- `recognize_blanks`'s
/// job, not this function's) is dropped rather than reported.
///
/// Every candidate this returns should be treated as lower-confidence
/// than a blank match or a [`crate::detect_candidates`] known-value
/// match -- both of those are unambiguous by construction (a blank has
/// an exact self-delimiting boundary; a known value is an exact literal
/// match), while this is a boundary *guess*. This function has no
/// notion of confidence tiers itself (same "pure matcher, no judgment
/// calls" scope as every other function here) -- the caller is
/// responsible for treating every candidate from this function as
/// always needing review, never auto-applied.
pub fn recognize_filled_values(
    document_text: &str,
    patterns: &[LabelProximityPattern],
) -> Vec<Candidate> {
    let other_boundaries = all_label_boundaries(document_text, patterns);
    let mut candidates = Vec::new();

    for pattern in patterns {
        for (label_start, label_end) in find_word_bounded_matches(document_text, &pattern.label) {
            if let Some(anchor) = &pattern.requires_preceding_anchor {
                if !anchor_precedes(document_text, label_start, anchor) {
                    continue;
                }
            }

            let span = match pattern.position {
                LabelPosition::After => value_span_after(
                    document_text,
                    label_end,
                    pattern.max_gap_chars,
                    &other_boundaries,
                ),
                LabelPosition::Before => value_span_before(
                    document_text,
                    label_start,
                    pattern.max_gap_chars,
                    &other_boundaries,
                ),
            };

            if let Some((start, end)) = span {
                candidates.push(Candidate {
                    tag_key: pattern.tag_key.clone(),
                    matched_text: document_text[start..end].to_string(),
                    start,
                    end,
                });
            }
        }
    }

    candidates.sort_by_key(|candidate| candidate.start);
    candidates
}

/// Every position in `document_text` where any pattern's label starts
/// or ends -- used as a boundary a filled-value guess must never cross,
/// so a value can't swallow an adjacent labeled field that happens to
/// share the same paragraph with no `\n`/`\t` between them.
fn all_label_boundaries(document_text: &str, patterns: &[LabelProximityPattern]) -> Vec<usize> {
    let mut boundaries = Vec::new();
    for pattern in patterns {
        for (start, end) in find_word_bounded_matches(document_text, &pattern.label) {
            boundaries.push(start);
            boundaries.push(end);
        }
    }
    boundaries.sort_unstable();
    boundaries
}

/// The value span starting at `from` (a label's end, for
/// [`LabelPosition::After`]), bounded by whichever of `\n`, `\t`,
/// another label's boundary, or `max_gap_chars` comes first, then
/// trimmed on both sides.
fn value_span_after(
    text: &str,
    from: usize,
    max_gap_chars: usize,
    other_boundaries: &[usize],
) -> Option<(usize, usize)> {
    // Skip the label's own connector punctuation (the tab in
    // `"FROM:\tNo Ka Oi..."`, or a colon/space with no tab at all)
    // *before* looking for the value's end boundary -- otherwise that
    // same connector tab gets found as if it were the boundary marking
    // the end of a (zero-width) value, never leaving room to skip past
    // it.
    let value_start = skip_leading_connectors(text, from);

    let window_end = char_boundary_at_or_after(text, (from + max_gap_chars).min(text.len()));
    if value_start >= window_end {
        return None;
    }

    // Unlike `recognize_blanks` (where `max_gap_chars` is only a
    // proximity cutoff on an already well-defined blank), a filled
    // value has no shape of its own -- so `end` must land on a REAL
    // boundary (paragraph break, tab, another label, or the literal end
    // of `text` itself), not just wherever the search budget happened
    // to run out. Falling back to an *arbitrary* mid-text cutoff would
    // silently truncate mid-word (e.g. "No K" out of "No Ka Oi Self
    // Storage" if the real boundary sat just past `max_gap_chars`) -- a
    // wrong answer that reads as a confident one. `window_end ==
    // text.len()` is different: there is no more text to have missed,
    // so it's as real a boundary as any of the others.
    let mut end = (window_end == text.len()).then_some(window_end);
    if let Some(rel) = text[value_start..window_end].find('\n') {
        end = Some(end.map_or(value_start + rel, |e: usize| e.min(value_start + rel)));
    }
    if let Some(rel) = text[value_start..window_end].find('\t') {
        end = Some(end.map_or(value_start + rel, |e: usize| e.min(value_start + rel)));
    }
    for &boundary in other_boundaries {
        if boundary > value_start && boundary < end.unwrap_or(window_end) {
            end = Some(boundary);
        }
    }
    let end = end?;

    trim_trailing(text, value_start, end)
}

/// Mirror of [`value_span_after`] for [`LabelPosition::Before`]: the
/// value span ending at `to` (a label's start), searching backward.
fn value_span_before(
    text: &str,
    to: usize,
    max_gap_chars: usize,
    other_boundaries: &[usize],
) -> Option<(usize, usize)> {
    // Same reasoning as `value_start` above, mirrored: skip the
    // connector immediately before the label (e.g. the space in
    // `"ABM PARKING SERVICE (Tenant)"`) before searching backward for
    // where the value itself begins.
    let value_end = skip_trailing_connectors(text, to);

    let window_start = char_boundary_at_or_after(text, to.saturating_sub(max_gap_chars));
    if window_start >= value_end {
        return None;
    }

    // Same reasoning as `value_span_after`: `start` must land on a real
    // boundary, not just fall back to the raw search-budget cutoff, or
    // a value could get silently truncated at its own front edge.
    // `window_start == 0` is the mirrored exception: the literal start
    // of `text` is as real a boundary as any other.
    let mut start = (window_start == 0).then_some(window_start);
    if let Some(rel) = text[window_start..value_end].rfind('\n') {
        start = Some(window_start + rel + 1);
    }
    if let Some(rel) = text[window_start..value_end].rfind('\t') {
        start = Some(start.map_or(window_start + rel + 1, |s: usize| {
            s.max(window_start + rel + 1)
        }));
    }
    for &boundary in other_boundaries {
        if boundary > start.unwrap_or(window_start) && boundary < value_end {
            start = Some(boundary);
        }
    }
    let start = start?;

    trim_leading(text, start, value_end)
}

/// The first index `>= from` that isn't an [`is_value_connector`]
/// character -- e.g. skips the tab in `"FROM:\tNo Ka Oi..."` (`from` is
/// the label's own end, right after the colon) to land on the "N".
fn skip_leading_connectors(text: &str, from: usize) -> usize {
    let mut start = from;
    while start < text.len() {
        let Some(c) = text[start..].chars().next() else {
            break;
        };
        if is_value_connector(c) {
            start += c.len_utf8();
        } else {
            break;
        }
    }
    start
}

/// Mirror of [`skip_leading_connectors`]: the last index `<= to` that
/// isn't an [`is_value_connector`] character, searching backward.
fn skip_trailing_connectors(text: &str, to: usize) -> usize {
    let mut end = to;
    while end > 0 {
        let Some(c) = text[..end].chars().next_back() else {
            break;
        };
        if is_value_connector(c) {
            end -= c.len_utf8();
        } else {
            break;
        }
    }
    end
}

/// Trims [`is_trailing_punctuation`] characters off the right of
/// `text[start..end]`, then rejects the result if it's too short, or if
/// it's an underscore run (a real blank -- [`recognize_blanks`]'s job).
fn trim_trailing(text: &str, start: usize, mut end: usize) -> Option<(usize, usize)> {
    while end > start {
        let c = text[start..end].chars().next_back()?;
        if is_trailing_punctuation(c) {
            end -= c.len_utf8();
        } else {
            break;
        }
    }
    accept_value_span(text, start, end)
}

/// Mirror of [`trim_trailing`] for the leading side, used by
/// [`value_span_before`] (whose connector-skipping already happened on
/// the trailing side via [`skip_trailing_connectors`], so only the
/// leading side -- the value's own start, which may run right up
/// against ordinary prose rather than a connector -- needs no
/// additional trim beyond what the boundary search already found).
fn trim_leading(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    accept_value_span(text, start, end)
}

/// Shared final acceptance check for both directions: reject a span
/// that's too short, or that's purely an underscore run (a real blank
/// -- [`recognize_blanks`]'s job, not this function's).
fn accept_value_span(text: &str, start: usize, end: usize) -> Option<(usize, usize)> {
    if end <= start || end - start < MIN_FILLED_VALUE_LEN {
        return None;
    }
    if text[start..end].chars().all(|c| c == '_') {
        return None;
    }

    Some((start, end))
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

    mod filled_values {
        use super::*;

        #[test]
        fn finds_a_filled_value_after_its_label_bounded_by_a_newline() {
            let text = "FROM:\tNo Ka Oi Self Storage\n269 E. Papa Place";
            let candidates = recognize_filled_values(
                text,
                &[pattern("f.name", "FROM:", LabelPosition::After, 40)],
            );

            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].tag_key, "f.name");
            assert_eq!(candidates[0].matched_text, "No Ka Oi Self Storage");
        }

        #[test]
        fn finds_a_filled_value_before_its_label() {
            let text = "ABM PARKING SERVICE (Tenant)";
            let candidates = recognize_filled_values(
                text,
                &[pattern("e.name", "(Tenant)", LabelPosition::Before, 25)],
            );

            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].tag_key, "e.name");
            assert_eq!(candidates[0].matched_text, "ABM PARKING SERVICE");
        }

        #[test]
        fn bounds_a_filled_value_at_a_tab_not_just_a_newline() {
            let text = "Total Amount Due\t0.00\nPlease send this amount";
            let candidates = recognize_filled_values(
                text,
                &[pattern(
                    "l.baldue",
                    "Total Amount Due",
                    LabelPosition::After,
                    10,
                )],
            );

            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].matched_text, "0.00");
        }

        #[test]
        fn bounds_a_filled_value_at_another_labels_position_with_no_separator_between_them() {
            // No newline or tab between the two fields -- only the "Unit"
            // label's own start position keeps "TO:"'s value from
            // swallowing it.
            let text = "TO: ABM PARKING SERVICE  # Unit 1000";
            let candidates = recognize_filled_values(
                text,
                &[
                    pattern("e.name", "TO:", LabelPosition::After, 40),
                    pattern("u.num", "Unit", LabelPosition::After, 10),
                ],
            );

            let name = candidates.iter().find(|c| c.tag_key == "e.name").unwrap();
            assert_eq!(name.matched_text, "ABM PARKING SERVICE  #");

            let unit = candidates.iter().find(|c| c.tag_key == "u.num").unwrap();
            assert_eq!(unit.matched_text, "1000");
        }

        #[test]
        fn respects_the_max_gap_cutoff() {
            let text = "FROM: this sentence is way too long to be a real gap before a value";
            let candidates = recognize_filled_values(
                text,
                &[pattern("f.name", "FROM:", LabelPosition::After, 5)],
            );

            assert!(candidates.is_empty());
        }

        #[test]
        fn never_reports_an_actual_blank_run_leaving_it_to_recognize_blanks() {
            let text = "FROM:\t____________\n269 E. Papa Place";
            let candidates = recognize_filled_values(
                text,
                &[pattern("f.name", "FROM:", LabelPosition::After, 40)],
            );

            assert!(candidates.is_empty());
        }

        #[test]
        fn reports_nothing_when_the_label_is_immediately_followed_by_another_label() {
            let text = "FROM:\nTO:\tABM PARKING SERVICE";
            let candidates = recognize_filled_values(
                text,
                &[pattern("f.name", "FROM:", LabelPosition::After, 40)],
            );

            assert!(candidates.iter().all(|c| c.tag_key != "f.name"));
        }

        #[test]
        fn trims_trailing_sentence_punctuation_but_keeps_internal_punctuation() {
            let text = "Dear ABM PARKING SERVICE.\nAccording to our records";
            let candidates = recognize_filled_values(
                text,
                &[pattern("e.name", "Dear", LabelPosition::After, 40)],
            );

            assert_eq!(candidates[0].matched_text, "ABM PARKING SERVICE");
        }

        #[test]
        fn respects_a_preceding_anchor_the_same_way_recognize_blanks_does() {
            let text = "ALTERNATE NAME: Jane Doe\nADDRESS: 123 Main St";
            // Built directly rather than via `anchored_pattern` -- that
            // helper hardcodes max_gap_chars: 5 (fine for the short
            // blanks its own recognize_blanks tests use), too small to
            // fit "123 Main St" (11 chars).
            let anchored = LabelProximityPattern {
                tag_key: "e.a.address".to_string(),
                label: "ADDRESS:".to_string(),
                position: LabelPosition::After,
                max_gap_chars: 30,
                requires_preceding_anchor: Some(PrecedingAnchor {
                    text: "ALTERNATE NAME".to_string(),
                    within_chars: 30,
                }),
            };

            let candidates = recognize_filled_values(text, &[anchored]);

            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].matched_text, "123 Main St");
        }

        #[test]
        fn reports_no_candidate_when_the_label_never_appears() {
            let text = "This document never mentions the facility name at all.";
            let candidates = recognize_filled_values(
                text,
                &[pattern("f.name", "FROM:", LabelPosition::After, 40)],
            );

            assert!(candidates.is_empty());
        }

        #[test]
        fn end_to_end_against_a_real_filled_late_notice_letter() {
            // The exact shape docx_surgeon::read_docx produces for a real
            // system-generated late-notice letter: paragraphs joined by
            // \n, tab-separated label/value pairs on the same line, no
            // underscore blanks anywhere -- the case that motivated this
            // function's existence.
            let text = "\tFROM:\tNo Ka Oi Self Storage\n269 E. Papa Place\n\
                         \tTO:\tABM PARKING SERVICE\n300 RODGERS BLDG  #30\n\
                         Dear ABM PARKING SERVICE  # Unit 1000\n\
                         According to our records, rent on your unit, Unit 1000, was due";

            let candidates = recognize_filled_values(
                text,
                &[
                    pattern("f.name", "FROM:", LabelPosition::After, 30),
                    pattern("e.name", "TO:", LabelPosition::After, 30),
                    pattern("u.num", "Unit", LabelPosition::After, 10),
                ],
            );

            assert!(candidates
                .iter()
                .any(|c| c.tag_key == "f.name" && c.matched_text == "No Ka Oi Self Storage"));
            assert!(candidates
                .iter()
                .any(|c| c.tag_key == "e.name" && c.matched_text == "ABM PARKING SERVICE"));
            // "Unit" also appears a second time, embedded mid-sentence in
            // "rent on your unit, Unit 1000, was due" -- correctly NOT
            // reported: the nearest real boundary from there is the next
            // paragraph break, many characters further on ("...was due
            // on 1/1/2027. It is now past due. Your rent, $303.66 per\n"
            // in the real letter), and reaching for it would produce a
            // wrong, oversized match rather than just "1000". Declining
            // to guess here is this function's whole point.
            assert!(candidates
                .iter()
                .any(|c| c.tag_key == "u.num" && c.matched_text == "1000"));
        }
    }
}
