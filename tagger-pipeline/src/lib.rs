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
//! only *builds* an [`Edit`] from an already-confirmed candidate and a
//! caller-supplied replacement -- applying it is still
//! `docx_surgeon::apply_edits`'s job, called explicitly by the caller.

use docx_surgeon::{Edit, FlatDocument, RegionRef};
use unitprep_template_tagger::{
    detect_candidates, recognize_blanks, Candidate, LabelProximityPattern, TagValue,
};

/// Which review tier a candidate falls into, per the design's tier-1/
/// tier-2/tier-3 split. Tier 3 ("no match, leave alone") is never a
/// value here -- it's the absence of any [`RegionCandidate`] for a
/// span, not something this type represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionCandidate {
    pub region: RegionRef,
    pub candidate: Candidate,
    pub tier: ConfidenceTier,
}

/// Runs both matchers -- literal-value matching against `values`
/// ([`detect_candidates`]), and label-proximity blank recognition
/// against `patterns` ([`recognize_blanks`]) -- across every region of
/// `doc`: the body, then every table cell in document order. Returns
/// every candidate found, each tagged with which region it came from
/// and its [`ConfidenceTier`].
///
/// Deliberately does not deduplicate or drop anything -- same "propose,
/// never modify, never silently collapse a judgment call" philosophy
/// both matchers already hold individually. Ambiguity (multiple
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
    collect_region(RegionRef::Body, &doc.body.text, values, patterns, &mut raw);
    for (i, cell) in doc.table_cells.iter().enumerate() {
        collect_region(
            RegionRef::TableCell(i),
            &cell.text,
            values,
            patterns,
            &mut raw,
        );
    }
    assign_tiers(raw)
}

fn collect_region(
    region: RegionRef,
    text: &str,
    values: &[TagValue],
    patterns: &[LabelProximityPattern],
    out: &mut Vec<(RegionRef, Candidate)>,
) {
    for candidate in detect_candidates(text, values) {
        out.push((region, candidate));
    }
    for candidate in recognize_blanks(text, patterns) {
        out.push((region, candidate));
    }
}

/// A span is unambiguous (tier `Auto`) if exactly one candidate in the
/// whole batch shares its exact `(region, start, end)`. Deliberately
/// O(n^2) -- a real document's candidate count is small enough (tens,
/// not thousands) that this is simpler and just as fast in practice as
/// building a hash key out of [`RegionRef`], which would need it to
/// implement `Hash` for no other reason than this one internal check.
fn assign_tiers(raw: Vec<(RegionRef, Candidate)>) -> Vec<RegionCandidate> {
    raw.iter()
        .map(|(region, candidate)| {
            let competing = raw
                .iter()
                .filter(|(r, c)| {
                    r == region && c.start == candidate.start && c.end == candidate.end
                })
                .count();
            let tier = if competing <= 1 {
                ConfidenceTier::Auto
            } else {
                ConfidenceTier::NeedsReview
            };
            RegionCandidate {
                region: *region,
                candidate: candidate.clone(),
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
    /// Replace the matched span outright -- the underscores (or
    /// whatever literal text detect_candidates matched) are gone,
    /// `replacement` is all that's left in their place.
    Replace,
    /// Keep the blank, with `replacement` landing in the middle of it --
    /// the underscores (or whatever else `matched_text` is) on either
    /// side stay, split as evenly as possible. If the blank is too
    /// short to fit `replacement` with anything left over on either
    /// side, this degrades to [`Self::Replace`] -- there's no
    /// meaningful "preserved blank" left to keep once the tag itself
    /// would fill the whole span or more.
    ///
    /// Only meaningful for a blank (`recognize_blanks`'s candidates) --
    /// centering a tag inside an already-filled value
    /// (`detect_candidates`'s candidates) would leave fragments of the
    /// old value on both sides of the new tag rather than replaced by
    /// it. The caller decides which candidates this applies to; this
    /// function has no notion of where a candidate came from.
    PreserveBlank,
}

/// Builds the [`Edit`] that would apply `candidate`'s substitution in
/// the given `style`. `replacement` is the caller's choice (typically
/// `{{tag_key}}`) -- this crate has no opinion on merge-tag templating
/// syntax, only on where the substitution goes.
pub fn to_edit(candidate: &RegionCandidate, replacement: String, style: SubstitutionStyle) -> Edit {
    let matched_len = candidate.candidate.end - candidate.candidate.start;

    let (flat_start, flat_end) = match style {
        SubstitutionStyle::Replace => (candidate.candidate.start, candidate.candidate.end),
        SubstitutionStyle::PreserveBlank if replacement.len() < matched_len => {
            let left_padding = (matched_len - replacement.len()) / 2;
            let start = candidate.candidate.start + left_padding;
            (start, start + replacement.len())
        }
        SubstitutionStyle::PreserveBlank => (candidate.candidate.start, candidate.candidate.end),
    };

    Edit {
        region: candidate.region,
        flat_start,
        flat_end,
        replacement,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docx_surgeon::{apply_edits, extract_flat_text};

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
        // label_proximity pattern's label -- both matchers legitimately
        // find something here, and both are reported.
        let xml = wrap(r#"<w:p><w:r><w:t>Unit No.: 204</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        let patterns = [label_pattern(
            "u.num",
            "Unit No.",
            unitprep_template_tagger::LabelPosition::After,
        )];

        let candidates = find_candidates(&doc, &[value("u.num", "204")], &patterns);

        // detect_candidates finds "204"; recognize_blanks finds nothing
        // here (no underscore run) -- so only the literal match shows,
        // proving the two matchers really did both run independently
        // rather than one silently suppressing the other.
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].candidate.matched_text, "204");
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

        let edit = to_edit(
            &candidates[0],
            "{{e.name}}".to_string(),
            SubstitutionStyle::Replace,
        );

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
                to_edit(
                    c,
                    format!("{{{{{}}}}}", c.candidate.tag_key),
                    SubstitutionStyle::Replace,
                )
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
        // padding split 9/9.
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

        let edit = to_edit(
            &candidates[0],
            "{{m.indate}}".to_string(),
            SubstitutionStyle::PreserveBlank,
        );
        let edited_xml = apply_edits(&xml, &doc, &[edit]).unwrap();
        let reflattened = extract_flat_text(&edited_xml).body;

        assert_eq!(
            reflattened.text,
            "Move-In Date: _________{{m.indate}}_________"
        );
    }

    #[test]
    fn preserve_blank_style_degrades_to_replace_when_the_blank_is_too_short() {
        // 6 underscores, "{{m.indate}}" is 12 chars -- no room to keep
        // any of the blank on either side, so it's just replaced.
        let xml = wrap(r#"<w:p><w:r><w:t>Move-In Date: ______</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        let patterns = [label_pattern(
            "m.indate",
            "Move-In Date",
            unitprep_template_tagger::LabelPosition::After,
        )];
        let candidates = find_candidates(&doc, &[], &patterns);

        let edit = to_edit(
            &candidates[0],
            "{{m.indate}}".to_string(),
            SubstitutionStyle::PreserveBlank,
        );
        let edited_xml = apply_edits(&xml, &doc, &[edit]).unwrap();
        let reflattened = extract_flat_text(&edited_xml).body;

        assert_eq!(reflattened.text, "Move-In Date: {{m.indate}}");
    }
}
