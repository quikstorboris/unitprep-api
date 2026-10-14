//! Wires `unitprep-template-tagger`'s two matchers and `docx-surgeon`'s
//! region model together: given a whole document's flattened regions,
//! finds every candidate across the body **and** every table cell,
//! assigns each a [`ConfidenceTier`], and builds the region-aware
//! [`Edit`] each one would need to be applied.
//!
//! Domain logic only, same "no I/O" scope as its two dependencies --
//! reading the actual `.docx` bytes, and any DB-backed pattern/tag
//! lookups, are the caller's job. This crate exists specifically
//! because neither dependency should know about the other:
//! `unitprep-template-tagger` is a pure text-matching engine with no
//! notion of "region," and `docx-surgeon` has no notion of what a
//! candidate is. Combining "run every matcher against every region" is
//! its own coherent piece of logic, not naturally either matcher's job
//! or the document reader's job.
//!
//! **Hard rule inherited from both dependencies: propose, never
//! modify.** [`find_candidates`] never applies anything; [`to_edit`]
//! only *builds* an [`AppliedEdit`] from an already-confirmed candidate
//! and a caller-supplied replacement -- applying it is still
//! `docx_surgeon::apply_all_edits`'s job, called explicitly by the
//! caller.

use docx_surgeon::{Edit, FlatDocument, FlatText, RegionRef, UnderlineEdit};
use unitprep_template_tagger::{
    detect_candidates, find_label_ends, is_container_tail_punctuation, recognize_blanks,
    recognize_filled_values, recognize_tab_blanks, recognize_textbox_blanks, Candidate,
    LabelPosition, LabelProximityPattern, TagValue, TAB_BLANK_TEXT,
};

/// Which review tier a candidate falls into, per the design's tier-1/
/// tier-2/tier-3 split. Tier 3 ("no match, leave alone") is never a
/// value here -- it's the absence of any [`RegionCandidate`] for a
/// span, not something this type represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ConfidenceTier {
    /// Exactly one candidate was found for this exact `(region, start,
    /// end)` span -- nothing else competes for it, so it's safe to
    /// apply without asking.
    Auto,
    /// Two or more candidates cover the exact same span (e.g. two
    /// patterns matching the same blank with different `tag_key`s, or a
    /// value match and a blank match happening to land on identical
    /// coordinates) -- a human has to pick.
    NeedsReview,
}

/// One [`Candidate`] plus which region of the document it was found in
/// -- a candidate's `start`/`end` are meaningless without knowing which
/// region's text they're relative to -- plus its computed
/// [`ConfidenceTier`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RegionCandidate {
    pub region: RegionRef,
    pub candidate: Candidate,
    pub tier: ConfidenceTier,
}

/// Runs all three matchers -- literal-value matching against `values`
/// ([`detect_candidates`]), label-proximity blank recognition against
/// `patterns` ([`recognize_blanks`]), and label-proximity *filled*-value
/// recognition against the same `patterns`
/// ([`recognize_filled_values`]) -- across every region of `doc`: the
/// body, then every table cell in document order. Returns every
/// candidate found, each tagged with which region it came from and its
/// [`ConfidenceTier`].
///
/// Deliberately does not deduplicate or drop anything -- same "propose,
/// never modify, never silently collapse a judgment call" philosophy
/// every matcher already holds individually. Ambiguity (multiple
/// candidates for the same exact span) is surfaced via `tier`, not by
/// picking a winner: every one of the competing candidates is still
/// returned, all marked [`ConfidenceTier::NeedsReview`], so a human
/// reviewer sees the real choice rather than this crate's guess at it.
pub fn find_candidates(
    doc: &FlatDocument,
    values: &[TagValue],
    patterns: &[LabelProximityPattern],
) -> Vec<RegionCandidate> {
    let mut raw = Vec::new();
    // Only a text box a value can actually go next to is worth proposing:
    // one that can grow to fit a tag (a tag appended to a box sized to
    // its label would be clipped), or one whose group shape has a drawn
    // blank square after it.
    let writable_boxes: Vec<(usize, usize)> = (0..doc.textboxes.len())
        .filter(|&i| doc.textbox_can_hold_value(i))
        .map(|i| (doc.textboxes[i].text_start, doc.textboxes[i].text_end))
        .collect();
    collect_region(
        RegionRef::Body,
        &doc.body,
        &writable_boxes,
        values,
        patterns,
        &mut raw,
    );
    for (i, cell) in doc.table_cells.iter().enumerate() {
        collect_region(
            RegionRef::TableCell(i),
            cell,
            &[],
            values,
            patterns,
            &mut raw,
        );
    }
    collect_empty_cell_blanks(doc, patterns, &mut raw);
    assign_tiers(raw)
}

fn collect_region(
    region: RegionRef,
    flat: &FlatText,
    text_boxes: &[(usize, usize)],
    values: &[TagValue],
    patterns: &[LabelProximityPattern],
    out: &mut Vec<RawCandidate>,
) {
    let text = flat.text.as_str();
    for candidate in detect_candidates(text, values) {
        out.push((region, candidate, Origin::Exact));
    }
    for candidate in recognize_blanks(text, patterns) {
        out.push((region, candidate, Origin::Exact));
    }

    // A blank drawn as an underlined tab, or the end of a floating text
    // box holding just a label, is as exact as an underscore run -- the
    // document itself marks it. But an *unanchored* pattern whose label
    // shows up at several places ("City:" for the occupant, the
    // alternate contact and the lienholder) cannot say which one it
    // means, so what it finds there is only a suggestion.
    let (anchored_or_unique, ambiguous): (Vec<_>, Vec<_>) =
        patterns.iter().cloned().partition(|pattern| {
            pattern.requires_preceding_anchor.is_some()
                || pattern.repeats
                || find_label_ends(text, pattern).len() <= 1
        });
    for (set, origin) in [
        (&anchored_or_unique, Origin::Exact),
        (&ambiguous, Origin::AmbiguousLabel),
    ] {
        for candidate in recognize_tab_blanks(text, &flat.underlined_tabs, set) {
            out.push((region, candidate, origin));
        }
        for candidate in recognize_textbox_blanks(text, text_boxes, set) {
            out.push((region, candidate, origin));
        }
    }

    // A filled-value guess is a boundary inference, not an exact match
    // -- see `recognize_filled_values`'s own doc comment. It always
    // needs review, regardless of whether anything else competes for
    // the same span.
    for candidate in recognize_filled_values(text, patterns) {
        out.push((region, candidate, Origin::Guess));
    }
}

/// How sure a matcher was about a candidate's span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// A blank or known value the document itself pins down.
    Exact,
    /// A boundary guess (`recognize_filled_values`).
    Guess,
    /// An exact blank, but from an unanchored pattern whose label
    /// appears at more than one place in the document.
    AmbiguousLabel,
}

type RawCandidate = (RegionRef, Candidate, Origin);

/// A table cell holding nothing at all, directly after a cell whose text
/// *ends with* a pattern's label, is that label's blank -- the shape of
/// an "initials" box (`label cell | empty cell | rest of the sentence`)
/// and of any two-column form whose value column is left empty.
///
/// The candidate is a zero-width point in the empty cell (`start ==
/// end == 0`, empty `matched_text`); applying it writes the tag as a new
/// run into the cell's empty paragraph (see
/// `docx_surgeon::EmptyParagraph`). Only [`LabelPosition::After`]
/// patterns apply -- the label comes first.
fn collect_empty_cell_blanks(
    doc: &FlatDocument,
    patterns: &[LabelProximityPattern],
    out: &mut Vec<RawCandidate>,
) {
    for index in 1..doc.table_cells.len() {
        let cell = &doc.table_cells[index];
        if !cell.is_empty_paragraph_insertion(0, 0) {
            continue;
        }
        let label_cell = &doc.table_cells[index - 1].text;

        for pattern in patterns {
            if pattern.position != LabelPosition::After {
                continue;
            }
            let ends_label = unitprep_template_tagger::find_label_ends(label_cell, pattern)
                .into_iter()
                .any(|label_end| {
                    label_cell[label_end..]
                        .chars()
                        .all(is_container_tail_punctuation)
                });
            if ends_label {
                out.push((
                    RegionRef::TableCell(index),
                    Candidate {
                        tag_key: pattern.tag_key.clone(),
                        matched_text: String::new(),
                        start: 0,
                        end: 0,
                    },
                    Origin::Exact,
                ));
            }
        }
    }
}

/// Drops every [`Origin::AmbiguousLabel`] candidate that shares its span
/// with an exact one -- the anchored pattern knows which section the
/// blank belongs to; the unanchored pattern only knows the label.
///
/// Nothing else is collapsed: two exact candidates for one span (even
/// with the same tag) are still both reported, so a reviewer sees the
/// real choice (see [`find_candidates`]).
fn drop_superseded_ambiguous(raw: Vec<RawCandidate>) -> Vec<RawCandidate> {
    let exact_spans: std::collections::HashSet<(RegionRef, usize, usize)> = raw
        .iter()
        .filter(|(_, _, origin)| *origin == Origin::Exact)
        .map(|(region, candidate, _)| (*region, candidate.start, candidate.end))
        .collect();

    raw.into_iter()
        .filter(|(region, candidate, origin)| {
            *origin != Origin::AmbiguousLabel
                || !exact_spans.contains(&(*region, candidate.start, candidate.end))
        })
        .collect()
}

/// A span is unambiguous (tier `Auto`) if exactly one candidate in the
/// whole batch shares its exact `(region, start, end)` *and* that
/// candidate is an exact match (see [`Origin`]). One pass builds
/// per-span counts, a second maps `raw` against them -- O(n) rather than
/// the previous O(n^2) re-scan of the whole batch per candidate, which
/// made a large or adversarial document's candidate count quadratic in
/// processing cost (see `MAX_CANDIDATES` at this crate's call site for
/// the accompanying hard cap).
fn assign_tiers(raw: Vec<RawCandidate>) -> Vec<RegionCandidate> {
    let raw = drop_superseded_ambiguous(raw);
    let mut counts: std::collections::HashMap<(RegionRef, usize, usize), usize> =
        std::collections::HashMap::new();
    for (region, candidate, _) in &raw {
        *counts
            .entry((*region, candidate.start, candidate.end))
            .or_insert(0) += 1;
    }

    raw.into_iter()
        .map(|(region, candidate, origin)| {
            let competing = counts[&(region, candidate.start, candidate.end)];
            let tier = if competing <= 1 && origin == Origin::Exact {
                ConfidenceTier::Auto
            } else {
                ConfidenceTier::NeedsReview
            };
            RegionCandidate {
                region,
                candidate,
                tier,
            }
        })
        .collect()
}

/// How a confirmed substitution's `replacement` text lands relative to
/// the matched span -- an OM-facing choice (some prefer a clean tagged
/// document with the blank gone entirely; others prefer keeping the
/// visual blank line, e.g. matching a signature-line convention already
/// seen in the corpus: `/s/{{e.name}}______________`), not something
/// this crate decides on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstitutionStyle {
    /// Replace the matched span outright. For a `detect_candidates`
    /// match (an already-filled value), `replacement` is all that's
    /// left in the value's place, with no formatting change -- a real
    /// value's width has no particular meaning to preserve.
    ///
    /// For a blank (`recognize_blanks`'s underscore-run candidates),
    /// the underscores are deleted and `replacement` is given underline
    /// formatting instead -- see [`docx_surgeon::UnderlineEdit`]'s doc
    /// comment for why an underline, rather than trying to match the
    /// blank's exact original width, is the right way to represent
    /// this: no text-level substitution can make a `{{tag_key}}`
    /// placeholder occupy exactly the width of the underscores it
    /// replaces (its own characters render at a different width, and
    /// its length is arbitrary anyway -- QMS's later real-value merge
    /// will insert something of a *different*, unrelated length in the
    /// very same spot). An underline reads correctly regardless of
    /// what ends up under it.
    Replace,
    /// Keep the blank, with `replacement` landing in the middle of it --
    /// the underscores (or whatever else `matched_text` is) on either
    /// side stay, split as evenly as possible. If the blank is too
    /// short to fit `replacement` with anything left over on either
    /// side, this degrades to [`Self::Replace`] -- there's no
    /// meaningful "preserved blank" left to keep once the tag itself
    /// would fill the whole span or more.
    ///
    /// For a blank, the inserted tag itself is *also* given underline
    /// formatting (same reasoning as [`Self::Replace`]'s own blank
    /// case) -- the surrounding kept underscores are untouched, but the
    /// tag sitting between them should still read as "this was a blank
    /// line," not as plain text that happens to have underscores on
    /// either side of it.
    ///
    /// Only meaningful for a blank (`recognize_blanks`'s candidates) --
    /// centering a tag inside an already-filled value
    /// (`detect_candidates`'s candidates) would leave fragments of the
    /// old value on both sides of the new tag rather than replaced by
    /// it, and there's no blank to underline in that case either. The
    /// caller decides which candidates this applies to; this function
    /// has no notion of where a candidate came from.
    PreserveBlank,
}

/// What [`to_edit`] built for one candidate -- which of `docx-surgeon`'s
/// two edit kinds depends on both `style` and whether the candidate is
/// a blank, so the caller needs to route each to the matching batch
/// (`docx_surgeon::apply_all_edits` takes both at once).
#[derive(Debug, Clone)]
pub enum AppliedEdit {
    Plain(Edit),
    Underline(UnderlineEdit),
}

/// Builds the edit that would apply `candidate`'s substitution in the
/// given `style`. `replacement` is the caller's choice (typically
/// `{{tag_key}}`) -- this crate has no opinion on merge-tag templating
/// syntax, only on where the substitution goes.
pub fn to_edit(
    candidate: &RegionCandidate,
    replacement: String,
    style: SubstitutionStyle,
) -> AppliedEdit {
    let blank_start = candidate.candidate.start;
    let blank_end = candidate.candidate.end;
    let matched_len = blank_end - blank_start;
    let is_blank = is_underscore_run(&candidate.candidate.matched_text);

    // A text-box label's value is appended after the label, so it needs
    // a separating space. (The same zero-width, empty-text shape in a
    // table cell is the cell's only content, and needs none.)
    let replacement = if candidate.region == RegionRef::Body
        && blank_start == blank_end
        && candidate.candidate.matched_text.is_empty()
    {
        format!(" {replacement}")
    } else {
        replacement
    };

    // An underlined-tab blank: the tag goes in just *before* the tab and
    // the tab (the form's drawn line) stays. The same for every style --
    // there is no underscore run to preserve or replace.
    if candidate.candidate.matched_text == TAB_BLANK_TEXT {
        return AppliedEdit::Plain(Edit {
            region: candidate.region,
            flat_start: blank_start,
            flat_end: blank_start,
            replacement,
        });
    }

    match style {
        SubstitutionStyle::Replace if is_blank => AppliedEdit::Underline(UnderlineEdit {
            region: candidate.region,
            flat_start: blank_start,
            flat_end: blank_end,
            replacement,
        }),
        SubstitutionStyle::Replace => AppliedEdit::Plain(Edit {
            region: candidate.region,
            flat_start: blank_start,
            flat_end: blank_end,
            replacement,
        }),
        SubstitutionStyle::PreserveBlank if replacement.len() < matched_len => {
            let left_padding = (matched_len - replacement.len()) / 2;
            let start = blank_start + left_padding;
            let end = start + replacement.len();
            if is_blank {
                AppliedEdit::Underline(UnderlineEdit {
                    region: candidate.region,
                    flat_start: start,
                    flat_end: end,
                    replacement,
                })
            } else {
                AppliedEdit::Plain(Edit {
                    region: candidate.region,
                    flat_start: start,
                    flat_end: end,
                    replacement,
                })
            }
        }
        SubstitutionStyle::PreserveBlank if is_blank => AppliedEdit::Underline(UnderlineEdit {
            region: candidate.region,
            flat_start: blank_start,
            flat_end: blank_end,
            replacement,
        }),
        SubstitutionStyle::PreserveBlank => AppliedEdit::Plain(Edit {
            region: candidate.region,
            flat_start: blank_start,
            flat_end: blank_end,
            replacement,
        }),
    }
}

/// Whether `matched_text` is a blank -- an underscore-run candidate
/// from [`recognize_blanks`], as opposed to a real already-filled value
/// from [`detect_candidates`]. Width-preserving padding only makes
/// sense for the former; a real value's matched text has no "blank" to
/// keep the shape of.
fn is_underscore_run(matched_text: &str) -> bool {
    !matched_text.is_empty() && matched_text.chars().all(|c| c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use docx_surgeon::{apply_all_edits, apply_edits, extract_flat_text};

    fn plain(edit: AppliedEdit) -> Edit {
        match edit {
            AppliedEdit::Plain(edit) => edit,
            AppliedEdit::Underline(_) => panic!("expected a plain Edit, got an UnderlineEdit"),
        }
    }

    fn underlined(edit: AppliedEdit) -> UnderlineEdit {
        match edit {
            AppliedEdit::Underline(edit) => edit,
            AppliedEdit::Plain(_) => panic!("expected an UnderlineEdit, got a plain Edit"),
        }
    }

    fn value(tag_key: &str, value: &str) -> TagValue {
        TagValue {
            tag_key: tag_key.to_string(),
            value: value.to_string(),
        }
    }

    fn label_pattern(
        tag_key: &str,
        label: &str,
        position: unitprep_template_tagger::LabelPosition,
    ) -> LabelProximityPattern {
        LabelProximityPattern {
            tag_key: tag_key.to_string(),
            label: label.to_string(),
            position,
            max_gap_chars: 5,
            requires_preceding_anchor: None,
            repeats: false,
        }
    }

    fn wrap(body: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#
        )
    }

    #[test]
    fn finds_a_known_value_in_the_body() {
        let xml = wrap(r#"<w:p><w:r><w:t>Tenant: John Smith</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let candidates = find_candidates(&doc, &[value("e.name", "John Smith")], &[]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].region, RegionRef::Body);
        assert_eq!(candidates[0].candidate.tag_key, "e.name");
    }

    #[test]
    fn finds_a_known_value_inside_a_table_cell() {
        let xml = wrap(
            r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>204</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        );
        let doc = extract_flat_text(&xml);

        let candidates = find_candidates(&doc, &[value("u.num", "204")], &[]);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].region, RegionRef::TableCell(0));
    }

    #[test]
    fn finds_a_label_proximity_blank_inside_a_table_cell() {
        let xml = wrap(
            r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Move-In Date: ______</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        );
        let doc = extract_flat_text(&xml);
        let patterns = [label_pattern(
            "m.indate",
            "Move-In Date",
            unitprep_template_tagger::LabelPosition::After,
        )];

        let candidates = find_candidates(&doc, &[], &patterns);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].region, RegionRef::TableCell(0));
        assert_eq!(candidates[0].candidate.tag_key, "m.indate");
    }

    #[test]
    fn reports_candidates_from_both_matchers_without_deduplicating() {
        // "204" is both a known value AND sits right after a
        // label_proximity pattern's label with nothing else nearby --
        // all three matchers legitimately find something here (detect_
        // candidates via the known value, recognize_filled_values via
        // the label proximity, recognize_blanks nothing since there's
        // no underscore run), and none suppress each other.
        let xml = wrap(r#"<w:p><w:r><w:t>Unit No.: 204</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        let patterns = [label_pattern(
            "u.num",
            "Unit No.",
            unitprep_template_tagger::LabelPosition::After,
        )];

        let candidates = find_candidates(&doc, &[value("u.num", "204")], &patterns);

        // Both real finds land on the exact same span, so this also
        // proves force_review kicks in even when there'd otherwise be
        // no ambiguity: without it, 2 identical-content candidates for
        // one span already means competing > 1 -> NeedsReview on its
        // own, so a dedicated single-filled-value-match tier assertion
        // lives in `a_filled_value_match_always_needs_review` below.
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(|c| c.candidate.matched_text == "204"));
        assert!(candidates
            .iter()
            .all(|c| c.tier == ConfidenceTier::NeedsReview));
    }

    #[test]
    fn a_filled_value_match_always_needs_review() {
        // No known values supplied (unlike the test above) -- "204" is
        // recognized ONLY via label proximity to an already-filled
        // value, with nothing else competing for the same span. Proves
        // force_review, not span competition, is what puts this in
        // NeedsReview: a single candidate with zero competitors would
        // otherwise be Auto by the same rule every other matcher's
        // candidates follow.
        let xml = wrap(r#"<w:p><w:r><w:t>Unit No.: 204</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        let patterns = [label_pattern(
            "u.num",
            "Unit No.",
            unitprep_template_tagger::LabelPosition::After,
        )];

        let candidates = find_candidates(&doc, &[], &patterns);

        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].candidate.matched_text, "204");
        assert_eq!(candidates[0].tier, ConfidenceTier::NeedsReview);
    }

    #[test]
    fn an_unambiguous_candidate_is_tier_auto() {
        let xml = wrap(r#"<w:p><w:r><w:t>Tenant: John Smith</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let candidates = find_candidates(&doc, &[value("e.name", "John Smith")], &[]);

        assert_eq!(candidates[0].tier, ConfidenceTier::Auto);
    }

    #[test]
    fn two_patterns_matching_the_same_blank_are_both_tier_needs_review() {
        let xml = wrap(r#"<w:p><w:r><w:t>Date: ______</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        let patterns = [
            label_pattern(
                "m.indate",
                "Date",
                unitprep_template_tagger::LabelPosition::After,
            ),
            label_pattern(
                "l.ptd",
                "Date",
                unitprep_template_tagger::LabelPosition::After,
            ),
        ];

        let candidates = find_candidates(&doc, &[], &patterns);

        assert_eq!(candidates.len(), 2);
        assert!(candidates
            .iter()
            .all(|c| c.tier == ConfidenceTier::NeedsReview));
    }

    #[test]
    fn ambiguity_in_one_region_does_not_affect_a_candidate_in_another() {
        let xml = wrap(&format!(
            "{}{}",
            r#"<w:p><w:r><w:t>Date: ______</w:t></w:r></w:p>"#,
            r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>204</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
        ));
        let doc = extract_flat_text(&xml);
        let patterns = [
            label_pattern(
                "m.indate",
                "Date",
                unitprep_template_tagger::LabelPosition::After,
            ),
            label_pattern(
                "l.ptd",
                "Date",
                unitprep_template_tagger::LabelPosition::After,
            ),
        ];

        let candidates = find_candidates(&doc, &[value("u.num", "204")], &patterns);

        let cell_candidate = candidates
            .iter()
            .find(|c| c.region == RegionRef::TableCell(0))
            .unwrap();
        assert_eq!(cell_candidate.tier, ConfidenceTier::Auto);
    }

    #[test]
    fn to_edit_builds_a_region_aware_edit_from_a_candidate() {
        let xml = wrap(r#"<w:p><w:r><w:t>Tenant: John Smith</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        let candidates = find_candidates(&doc, &[value("e.name", "John Smith")], &[]);

        let edit = plain(to_edit(
            &candidates[0],
            "{{e.name}}".to_string(),
            SubstitutionStyle::Replace,
        ));

        assert_eq!(edit.region, RegionRef::Body);
        assert_eq!(edit.flat_start, candidates[0].candidate.start);
        assert_eq!(edit.replacement, "{{e.name}}");
    }

    #[test]
    fn end_to_end_find_and_apply_across_body_and_a_table_cell() {
        let xml = wrap(&format!(
            "{}{}",
            r#"<w:p><w:r><w:t>Tenant: John Smith</w:t></w:r></w:p>"#,
            r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>204</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#
        ));
        let doc = extract_flat_text(&xml);
        let values = [value("e.name", "John Smith"), value("u.num", "204")];

        let candidates = find_candidates(&doc, &values, &[]);
        assert_eq!(candidates.len(), 2);

        let edits: Vec<Edit> = candidates
            .iter()
            .map(|c| {
                plain(to_edit(
                    c,
                    format!("{{{{{}}}}}", c.candidate.tag_key),
                    SubstitutionStyle::Replace,
                ))
            })
            .collect();

        let edited_xml = apply_edits(&xml, &doc, &edits).unwrap();
        let reflattened = extract_flat_text(&edited_xml);

        assert_eq!(reflattened.body.text, "Tenant: {{e.name}}");
        assert_eq!(reflattened.table_cells[0].text, "{{u.num}}");
    }

    #[test]
    fn preserve_blank_style_centers_the_tag_with_underscores_on_both_sides() {
        // 30 underscores, "{{m.indate}}" is 12 chars -- 18 chars of
        // padding split 9/9. The kept underscores stay plain; only the
        // tag itself in the middle is underlined.
        let xml = wrap(
            r#"<w:p><w:r><w:t>Move-In Date: ______________________________</w:t></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);
        let patterns = [label_pattern(
            "m.indate",
            "Move-In Date",
            unitprep_template_tagger::LabelPosition::After,
        )];
        let candidates = find_candidates(&doc, &[], &patterns);

        let edit = underlined(to_edit(
            &candidates[0],
            "{{m.indate}}".to_string(),
            SubstitutionStyle::PreserveBlank,
        ));
        let edited_xml = apply_all_edits(&xml, &doc, &[], &[edit]).unwrap();
        let reflattened = extract_flat_text(&edited_xml).body;

        assert_eq!(
            reflattened.text,
            "Move-In Date: _________{{m.indate}}_________"
        );
        assert!(
            edited_xml.contains(r#"<w:rPr><w:u w:val="single"/></w:rPr><w:t>{{m.indate}}</w:t>"#)
        );
    }

    #[test]
    fn replace_style_deletes_a_blanks_underscores_and_underlines_the_tag() {
        let xml = wrap(r#"<w:p><w:r><w:t>SIZE___________________________CITY</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        let patterns = [label_pattern(
            "u.dim",
            "SIZE",
            unitprep_template_tagger::LabelPosition::After,
        )];
        let candidates = find_candidates(&doc, &[], &patterns);

        let edit = underlined(to_edit(
            &candidates[0],
            "{{u.dim}}".to_string(),
            SubstitutionStyle::Replace,
        ));
        let edited_xml = apply_all_edits(&xml, &doc, &[], &[edit]).unwrap();
        let reflattened = extract_flat_text(&edited_xml).body;

        assert_eq!(reflattened.text, "SIZE{{u.dim}}CITY");
        assert!(edited_xml.contains(r#"<w:rPr><w:u w:val="single"/></w:rPr><w:t>{{u.dim}}</w:t>"#));
    }

    #[test]
    fn replace_style_does_not_pad_an_already_filled_value() {
        // "Atherton Storage" (16 chars) is a real detect_candidates
        // match, not a blank -- Replace must not start padding real
        // values with spaces just because the tag is shorter.
        let xml = wrap(r#"<w:p><w:r><w:t>Tenant: Atherton Storage</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        let candidates = find_candidates(&doc, &[value("f.name", "Atherton Storage")], &[]);

        let edit = plain(to_edit(
            &candidates[0],
            "{{f.name}}".to_string(),
            SubstitutionStyle::Replace,
        ));
        let edited_xml = apply_edits(&xml, &doc, &[edit]).unwrap();
        let reflattened = extract_flat_text(&edited_xml).body;

        assert_eq!(reflattened.text, "Tenant: {{f.name}}");
    }

    #[test]
    fn preserve_blank_style_degrades_to_replace_when_the_blank_is_too_short() {
        // 6 underscores, "{{m.indate}}" is 12 chars -- no room to keep
        // any of the blank on either side, so it's just replaced, same
        // as Replace's own blank case (underlined, not plain).
        let xml = wrap(r#"<w:p><w:r><w:t>Move-In Date: ______</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        let patterns = [label_pattern(
            "m.indate",
            "Move-In Date",
            unitprep_template_tagger::LabelPosition::After,
        )];
        let candidates = find_candidates(&doc, &[], &patterns);

        let edit = underlined(to_edit(
            &candidates[0],
            "{{m.indate}}".to_string(),
            SubstitutionStyle::PreserveBlank,
        ));
        let edited_xml = apply_all_edits(&xml, &doc, &[], &[edit]).unwrap();
        let reflattened = extract_flat_text(&edited_xml).body;

        assert_eq!(reflattened.text, "Move-In Date: {{m.indate}}");
    }

    #[test]
    fn preserve_blank_style_does_not_underline_an_already_filled_value() {
        // "Smith" (5 chars) is a real detect_candidates match, not a
        // blank; "{{e.lname}}" (11 chars) is longer than it, so this
        // hits PreserveBlank's own too-short-to-center fallback (a full
        // replace, same as Replace's shape) -- but since there's no
        // blank here, it must stay a plain Edit, not underlined.
        let xml = wrap(r#"<w:p><w:r><w:t>Last name: Smith</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        let candidates = find_candidates(&doc, &[value("e.lname", "Smith")], &[]);

        let edit = plain(to_edit(
            &candidates[0],
            "{{e.lname}}".to_string(),
            SubstitutionStyle::PreserveBlank,
        ));
        let edited_xml = apply_edits(&xml, &doc, &[edit]).unwrap();
        let reflattened = extract_flat_text(&edited_xml).body;

        assert_eq!(reflattened.text, "Last name: {{e.lname}}");
    }

    fn pattern_after(tag_key: &str, label: &str, max_gap_chars: usize) -> LabelProximityPattern {
        LabelProximityPattern {
            tag_key: tag_key.to_string(),
            label: label.to_string(),
            position: unitprep_template_tagger::LabelPosition::After,
            max_gap_chars,
            requires_preceding_anchor: None,
            repeats: false,
        }
    }

    const UNDERLINED_TAB_FORM: &str = concat!(
        r#"<w:p><w:r><w:t>DATE OF LEASE: </w:t></w:r>"#,
        r#"<w:r><w:rPr><w:u w:val="single"/></w:rPr><w:tab/></w:r>"#,
        r#"<w:r><w:t> Access Code: </w:t></w:r>"#,
        r#"<w:r><w:rPr><w:u w:val="single"/></w:rPr><w:tab/></w:r></w:p>"#,
        r#"<w:p><w:r><w:t>Late fee</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>$20</w:t></w:r></w:p>"#,
    );

    #[test]
    fn an_underlined_tab_blank_is_found_and_the_tab_survives_the_edit() {
        let xml = wrap(UNDERLINED_TAB_FORM);
        let doc = extract_flat_text(&xml);
        let patterns = [
            pattern_after("m.indate", "DATE OF LEASE", 5),
            pattern_after("e.keycodes", "Access Code", 5),
            pattern_after("x.fee", "Late fee", 5),
        ];

        let found = find_candidates(&doc, &[], &patterns);

        // The plain tab after "Late fee" is a separator, not a blank --
        // at most the filled-value guesser (always NeedsReview) sees
        // "$20" there.
        let blanks: Vec<&RegionCandidate> = found
            .iter()
            .filter(|c| c.tier == ConfidenceTier::Auto)
            .collect();
        let keys: Vec<&str> = blanks
            .iter()
            .map(|c| c.candidate.tag_key.as_str())
            .collect();
        assert_eq!(keys, vec!["m.indate", "e.keycodes"]);

        let edits: Vec<Edit> = blanks
            .iter()
            .map(|c| {
                plain(to_edit(
                    c,
                    format!("{{{{{}}}}}", c.candidate.tag_key),
                    SubstitutionStyle::Replace,
                ))
            })
            .collect();
        let result = apply_all_edits(&xml, &doc, &edits, &[]).unwrap();

        assert!(result.contains("<w:t>DATE OF LEASE: {{m.indate}}</w:t></w:r><w:r><w:rPr><w:u w:val=\"single\"/></w:rPr><w:tab/></w:r>"));
        assert!(result.contains("<w:t> Access Code: {{e.keycodes}}</w:t></w:r><w:r><w:rPr><w:u w:val=\"single\"/></w:rPr><w:tab/></w:r>"));
        assert_eq!(result.matches("<w:tab/>").count(), 3);
    }

    #[test]
    fn preserve_blank_style_treats_a_tab_blank_the_same() {
        let xml = wrap(UNDERLINED_TAB_FORM);
        let doc = extract_flat_text(&xml);
        let found = find_candidates(&doc, &[], &[pattern_after("m.indate", "DATE OF LEASE", 5)]);

        let edit = plain(to_edit(
            &found[0],
            "{{m.indate}}".to_string(),
            SubstitutionStyle::PreserveBlank,
        ));

        assert_eq!(edit.flat_start, edit.flat_end);
    }

    const INITIALS_ROW: &str = concat!(
        r#"<w:tbl><w:tr>"#,
        r#"<w:tc><w:p><w:r><w:t>By placing his/her initials here,</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:p></w:p></w:tc>"#,
        r#"<w:tc><w:p><w:r><w:t>, Occupant acknowledges</w:t></w:r></w:p></w:tc>"#,
        r#"</w:tr></w:tbl>"#,
    );

    #[test]
    fn an_empty_cell_after_a_cell_ending_in_a_label_is_that_labels_blank() {
        let xml = wrap(INITIALS_ROW);
        let doc = extract_flat_text(&xml);

        let found = find_candidates(&doc, &[], &[pattern_after("e.init", "initials here", 5)]);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].region, RegionRef::TableCell(1));
        assert_eq!(found[0].tier, ConfidenceTier::Auto);

        let edit = plain(to_edit(
            &found[0],
            "{{e.init}}".to_string(),
            SubstitutionStyle::Replace,
        ));
        let result = apply_all_edits(&xml, &doc, &[edit], &[]).unwrap();
        assert!(result.contains(
            r#"<w:tc><w:p><w:r><w:t xml:space="preserve">{{e.init}}</w:t></w:r></w:p></w:tc>"#
        ));
    }

    #[test]
    fn an_empty_cell_after_a_cell_that_merely_mentions_the_label_is_not_a_blank() {
        let xml = wrap(concat!(
            r#"<w:tbl><w:tr>"#,
            r#"<w:tc><w:p><w:r><w:t>Initials here are optional for minors</w:t></w:r></w:p></w:tc>"#,
            r#"<w:tc><w:p></w:p></w:tc>"#,
            r#"</w:tr></w:tbl>"#,
        ));
        let doc = extract_flat_text(&xml);

        assert!(
            find_candidates(&doc, &[], &[pattern_after("e.init", "initials here", 5)]).is_empty()
        );
    }

    #[test]
    fn a_non_empty_cell_after_the_label_cell_is_not_an_empty_cell_blank() {
        let xml = wrap(concat!(
            r#"<w:tbl><w:tr>"#,
            r#"<w:tc><w:p><w:r><w:t>Initials here:</w:t></w:r></w:p></w:tc>"#,
            r#"<w:tc><w:p><w:r><w:t>JS</w:t></w:r></w:p></w:tc>"#,
            r#"</w:tr></w:tbl>"#,
        ));
        let doc = extract_flat_text(&xml);

        assert!(
            find_candidates(&doc, &[], &[pattern_after("e.init", "initials here", 5)]).is_empty()
        );
    }

    fn text_box_xml(h_pt: i64, label: &str) -> String {
        format!(
            concat!(
                r#"<w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:drawing><wp:anchor>"#,
                r#"<wp:positionH relativeFrom="page"><wp:posOffset>{h}</wp:posOffset></wp:positionH>"#,
                r#"<wp:positionV relativeFrom="paragraph"><wp:posOffset>3810</wp:posOffset></wp:positionV>"#,
                r#"<wp:extent cx="500000" cy="106680"/><a:graphic><a:graphicData><wps:wsp><wps:spPr>"#,
                r#"<a:xfrm><a:off x="0" y="0"/><a:ext cx="500000" cy="106680"/></a:xfrm></wps:spPr>"#,
                r#"<wps:txbx><w:txbxContent><w:p><w:r><w:t>{label}</w:t></w:r></w:p></w:txbxContent></wps:txbx>"#,
                r#"</wps:wsp></a:graphicData></a:graphic></wp:anchor></w:drawing></mc:Choice>"#,
                r#"<mc:Fallback><w:pict><w:txbxContent><w:p><w:r><w:t>{label}</w:t></w:r></w:p></w:txbxContent></w:pict></mc:Fallback>"#,
                r#"</mc:AlternateContent></w:r>"#,
            ),
            h = h_pt * 12_700,
            label = label
        )
    }

    fn text_box_form() -> String {
        wrap(&format!(
            concat!(
                "<w:p>{first}{last}</w:p>",
                r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:right="720"/></w:sectPr>"#,
            ),
            first = text_box_xml(47, "First:"),
            last = text_box_xml(220, "Last:"),
        ))
    }

    #[test]
    fn a_text_box_label_gets_its_tag_and_the_box_grows_to_the_next_box() {
        let xml = text_box_form();
        let doc = extract_flat_text(&xml);
        let patterns = [
            pattern_after("e.fname", "First:", 5),
            pattern_after("e.lname", "Last:", 5),
        ];

        let found = find_candidates(&doc, &[], &patterns);

        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|c| c.tier == ConfidenceTier::Auto));

        let edits: Vec<Edit> = found
            .iter()
            .map(|c| {
                plain(to_edit(
                    c,
                    format!("{{{{{}}}}}", c.candidate.tag_key),
                    SubstitutionStyle::Replace,
                ))
            })
            .collect();
        let result = apply_all_edits(&xml, &doc, &edits, &[]).unwrap();

        assert!(result.contains("<w:t>First: {{e.fname}}</w:t>"));
        assert!(result.contains("<w:t>Last: {{e.lname}}</w:t>"));
        // "First:" (47pt) grows to 2pt short of "Last:" (220pt); "Last:" to the 576pt margin.
        assert!(result.contains(r#"<wp:extent cx="2171700" cy="106680"/>"#));
        assert!(result.contains(r#"<wp:extent cx="4521200" cy="106680"/>"#));
        // The legacy copies are left exactly as they were.
        assert!(result.contains("<w:t>First:</w:t>"));
    }

    #[test]
    fn a_text_box_with_no_room_to_grow_is_not_proposed() {
        let xml = wrap(&format!(
            concat!(
                "<w:p>{first}{last}</w:p>",
                r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:right="720"/></w:sectPr>"#,
            ),
            first = text_box_xml(47, "First:"),
            last = text_box_xml(60, "Last:"),
        ));
        let doc = extract_flat_text(&xml);

        let found = find_candidates(&doc, &[], &[pattern_after("e.fname", "First:", 5)]);

        assert!(found.is_empty(), "{found:?}");
    }

    fn boxes_form(boxes: &[(i64, &str)]) -> String {
        let anchored: String = boxes
            .iter()
            .map(|(h, label)| text_box_xml(*h, label))
            .collect();
        wrap(&format!(
            concat!(
                "<w:p>{}</w:p>",
                r#"<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:right="720"/></w:sectPr>"#,
            ),
            anchored
        ))
    }

    fn anchored(tag_key: &str, label: &str, anchor: &str) -> LabelProximityPattern {
        LabelProximityPattern {
            requires_preceding_anchor: Some(unitprep_template_tagger::PrecedingAnchor {
                text: anchor.to_string(),
                within_chars: 20,
            }),
            ..pattern_after(tag_key, label, 5)
        }
    }

    #[test]
    fn an_unanchored_label_that_repeats_is_only_a_suggestion() {
        let xml = boxes_form(&[(47, "City:"), (220, "City:")]);
        let doc = extract_flat_text(&xml);

        let found = find_candidates(&doc, &[], &[pattern_after("e.city", "City:", 5)]);

        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|c| c.tier == ConfidenceTier::NeedsReview));
    }

    #[test]
    fn an_unanchored_label_that_appears_once_is_still_automatic() {
        let xml = boxes_form(&[(47, "City:"), (220, "Zip:")]);
        let doc = extract_flat_text(&xml);

        let found = find_candidates(&doc, &[], &[pattern_after("e.city", "City:", 5)]);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].tier, ConfidenceTier::Auto);
    }

    #[test]
    fn an_anchored_pattern_supersedes_the_generic_one_on_the_same_blank() {
        let xml = boxes_form(&[(47, "City:"), (220, "Alt:"), (400, "City:")]);
        let doc = extract_flat_text(&xml);
        let patterns = [
            pattern_after("e.city", "City:", 5),
            anchored("e.a.city", "City:", "Alt:"),
        ];

        let found = find_candidates(&doc, &[], &patterns);

        let mut by_tag: Vec<(&str, ConfidenceTier)> = found
            .iter()
            .map(|c| (c.candidate.tag_key.as_str(), c.tier))
            .collect();
        by_tag.sort_by_key(|(tag, _)| *tag);
        // The first "City:" has no anchor, so only the generic guess;
        // the second is claimed by the anchored pattern alone.
        assert_eq!(
            by_tag,
            vec![
                ("e.a.city", ConfidenceTier::Auto),
                ("e.city", ConfidenceTier::NeedsReview)
            ]
        );
    }

    #[test]
    fn a_blank_split_across_runs_is_replaced_by_one_underlined_tag() {
        let xml = wrap(concat!(
            r#"<w:p><w:r><w:t>Date: </w:t></w:r>"#,
            r#"<w:r><w:rPr><w:b/></w:rPr><w:t>____</w:t></w:r><w:r><w:t>____</w:t></w:r></w:p>"#,
        ));
        let doc = extract_flat_text(&xml);
        let found = find_candidates(&doc, &[], &[pattern_after("d.now", "Date:", 5)]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].candidate.matched_text, "________");

        for style in [SubstitutionStyle::Replace, SubstitutionStyle::PreserveBlank] {
            let edit = underlined(to_edit(&found[0], "{{d.now}}".to_string(), style));
            let result = apply_all_edits(&xml, &doc, &[], &[edit]).unwrap();

            assert_eq!(result.matches("{{d.now}}").count(), 1, "{style:?}");
            // The run that holds the tag is underlined, and keeps the
            // original run's bold.
            let tag_run_start = result[..result.find("{{d.now}}").unwrap()]
                .rfind("<w:r>")
                .unwrap();
            let tag_run = &result[tag_run_start..result.find("{{d.now}}").unwrap()];
            assert!(
                tag_run.contains(r#"<w:u w:val="single"/>"#),
                "{style:?}: {tag_run}"
            );
        }
    }

    #[test]
    fn a_label_declared_repeatable_stays_automatic_when_it_recurs() {
        let xml = boxes_form(&[(47, "Initials here,"), (220, "Initials here,")]);
        let doc = extract_flat_text(&xml);
        let pattern = LabelProximityPattern {
            repeats: true,
            ..pattern_after("e.init", "Initials here", 5)
        };

        let found = find_candidates(&doc, &[], &[pattern]);

        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|c| c.tier == ConfidenceTier::Auto));
    }
}
