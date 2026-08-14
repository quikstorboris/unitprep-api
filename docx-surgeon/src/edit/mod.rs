use crate::read::{FlatDocument, RegionRef};

mod fragment;
mod overlap;
mod run_xml;

use fragment::{
    collect_fragments, compute_plain_replacements, compute_underline_replacements,
    finalize_fragment_replacements, splice_replacements, RunStyle, StyledFragment,
};
use overlap::{check_no_cross_kind_overlap, check_no_overlapping_spans, check_no_overlaps};

/// One text replacement, expressed in flat-text coordinates relative to
/// `region` (the same coordinate space [`crate::read::extract_flat_text`]
/// returns for that region). Two edits in different regions may
/// legitimately share the same numeric `flat_start`/`flat_end` -- they
/// address different physical text entirely.
#[derive(Debug, Clone)]
pub struct Edit {
    pub region: RegionRef,
    pub flat_start: usize,
    pub flat_end: usize,
    pub replacement: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EditError {
    /// The edit's coordinates touch no run at all -- refused rather
    /// than guessed at. This is distinct from spanning multiple runs
    /// (which `apply_edits` now handles by splicing across them); this
    /// is coordinates that don't correspond to any real text position,
    /// which should only happen from a genuinely stale or malformed
    /// `Edit`.
    NoMatchingRun {
        region: RegionRef,
        flat_start: usize,
        flat_end: usize,
    },
    /// Two edits in the *same region* overlap in flat-text coordinates.
    /// Edits in different regions never conflict with each other,
    /// regardless of their numeric coordinates -- see [`Edit`]'s doc
    /// comment.
    OverlappingEdits {
        region: RegionRef,
        first: (usize, usize),
        second: (usize, usize),
    },
}

/// Applies every edit to `document_xml`, returning the modified XML.
///
/// An edit may span more than one run -- a blank's underscore run can
/// be split across several `<w:t>` elements in real documents (a
/// formatting change mid-run, a spell-check restart point, anything
/// that gives Word a reason to end one run and start another) even
/// though it reads as one unbroken blank on screen. When that happens,
/// the first touched run keeps its own lead-in text and gets the edit's
/// full replacement appended; every run fully consumed by the edit
/// (including the touched portion of the last one) is emptied rather
/// than deleted outright -- the run *element* survives with empty
/// `<w:t>` content, so no formatting attribute anywhere is discarded,
/// only the text inside the affected runs changes.
///
/// A run touched by one or more edits always has its *entire* raw XML
/// text content replaced wholesale (decode -> splice in decoded space
/// -> re-encode -> replace the whole `<w:t>...</w:t>` content), even
/// when an edit only touches part of that run's text. This is
/// deliberate, not a shortcut: slicing raw XML bytes mid-entity
/// (`&amp;` is 5 bytes, decodes to 1) would silently corrupt the
/// document the moment a run's text contains one. Replacing the whole
/// run's content atomically after a decode/re-encode round trip makes
/// that failure mode impossible by construction.
///
/// Every byte of `document_xml` outside a targeted run's `<w:t>...
/// </w:t>` content is copied through unchanged -- this is what makes
/// the result safe to trust: nothing else in the file can have moved.
///
/// Edits may target any region of `doc` (the body or any table cell)
/// in the same call -- every [`crate::read::RunSpan`], regardless of
/// which region collected it, carries an absolute offset into this
/// same `document_xml`, so one combined splice pass is always correct
/// regardless of how many regions are involved.
pub fn apply_edits(
    document_xml: &str,
    doc: &FlatDocument,
    edits: &[Edit],
) -> Result<String, EditError> {
    let replacements = compute_plain_replacements(doc, edits)?;
    Ok(splice_replacements(document_xml, replacements))
}

/// Applies both a batch of ordinary [`Edit`]s and a batch of
/// [`UnderlineEdit`]s to `document_xml` in one combined pass. This is
/// not the same as calling [`apply_edits`] and then
/// [`apply_underline_edits`] in sequence on each other's output -- both
/// kinds of edit compute their replacements against the *same*
/// original `document_xml`'s byte offsets, and those offsets would go
/// stale the moment an intermediate splice changed the document's
/// length. Combining them here, before either's replacements are
/// spliced in, is what makes it safe for one `apply` call to carry a
/// mix of preserve-blank, underline, and plain-value substitutions.
///
/// Critically, this does *not* just concatenate
/// [`compute_plain_replacements`] and [`compute_underline_replacements`]'s
/// two independently-computed replacement lists -- a real document
/// routinely has several blanks sharing one physical run (Word doesn't
/// give each label-and-blank pair its own run just because a
/// higher-level tool would find that convenient), so a plain edit and
/// an underline edit can easily both touch the *same* run. Computed
/// independently, the plain side would replace only that run's `<w:t>`
/// content while the underline side replaced the run's whole element,
/// producing two overlapping byte ranges that corrupt the splice.
/// Routing everything through one shared per-run fragment
/// reconstruction (a plain edit's replacement text simply becomes an
/// "original-style" fragment, same shape as an underline edit's
/// underlined fragment) makes that structurally impossible instead of
/// merely unlikely.
pub fn apply_all_edits(
    document_xml: &str,
    doc: &FlatDocument,
    edits: &[Edit],
    underline_edits: &[UnderlineEdit],
) -> Result<String, EditError> {
    check_no_overlaps(edits)?;
    let underline_spans: Vec<(RegionRef, usize, usize)> = underline_edits
        .iter()
        .map(|e| (e.region, e.flat_start, e.flat_end))
        .collect();
    check_no_overlapping_spans(&underline_spans)?;
    check_no_cross_kind_overlap(edits, underline_edits)?;

    let mut by_run: Vec<(RegionRef, usize, Vec<StyledFragment>)> = Vec::new();
    for edit in edits {
        collect_fragments(
            doc,
            edit.region,
            edit.flat_start,
            edit.flat_end,
            &edit.replacement,
            RunStyle::Original,
            &mut by_run,
        )?;
    }
    for edit in underline_edits {
        collect_fragments(
            doc,
            edit.region,
            edit.flat_start,
            edit.flat_end,
            &edit.replacement,
            RunStyle::Underlined,
            &mut by_run,
        )?;
    }

    Ok(splice_replacements(
        document_xml,
        finalize_fragment_replacements(document_xml, doc, by_run),
    ))
}

/// One "delete the blank, underline the replacement" edit: every
/// original character in `[flat_start, flat_end)` is removed and
/// replaced outright by `replacement`, which is given underline
/// formatting so it still reads as a filled-in blank -- the same visual
/// convention several real templates in this corpus already use for
/// blanks drawn without any literal underscore characters at all (a
/// run's own `<w:u>` formatting, invisible to plain-text matching, is
/// exactly what [`crate::read::RunSpan::is_underlined`] exists to
/// detect on the *recognition* side).
///
/// This exists because no text-level substitution can make a
/// `{{tag_key}}` placeholder occupy exactly the width of the
/// underscores it replaces -- a placeholder's own characters (braces,
/// letters) render at a different width than underscores in every font
/// checked, and more fundamentally, the placeholder's own length is
/// arbitrary anyway (QMS's later real-value merge will insert an actual
/// value of its own unrelated length in the very same spot). Chasing
/// exact width preservation for a value nobody can predict the length
/// of is the wrong goal. Underlining the replacement instead sidesteps
/// the problem rather than solving it: an underline extends under
/// whatever text ends up there, so it reads correctly whether that text
/// is a short placeholder, a long one, or -- once QMS performs its own
/// merge and (like any reasonable template engine, including this
/// crate's own substitution model) carries the run's formatting forward
/// onto the value it inserts -- the real, variable-length value itself.
#[derive(Debug, Clone)]
pub struct UnderlineEdit {
    pub region: RegionRef,
    pub flat_start: usize,
    pub flat_end: usize,
    pub replacement: String,
}

/// Applies every [`UnderlineEdit`] to `document_xml`, returning the
/// modified XML.
///
/// Unlike [`apply_edits`], a run touched by an `UnderlineEdit` is not
/// necessarily left as one run -- when only *part* of a run falls
/// inside the replaced span (the label and blank sharing one run, e.g.
/// the real `"DATE: _"` case, where the label must stay in its original
/// formatting and only the blank's own text gets replaced-and-
/// underlined), that run is rebuilt as multiple sibling `<w:r>`
/// elements, one per contiguous original/underlined stretch, each
/// cloning the original run's `<w:rPr>` (adding a `<w:u>` override only
/// for the underlined fragment). A run entirely outside the edit's span
/// is passed through byte-for-byte unchanged.
///
/// Every byte of `document_xml` outside a touched run's own
/// `[run_start, run_end)` is copied through unchanged.
pub fn apply_underline_edits(
    document_xml: &str,
    doc: &FlatDocument,
    edits: &[UnderlineEdit],
) -> Result<String, EditError> {
    let replacements = compute_underline_replacements(document_xml, doc, edits)?;
    Ok(splice_replacements(document_xml, replacements))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::read::extract_flat_text;

    fn wrap(body: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#
        )
    }

    fn body_edit(flat_start: usize, flat_end: usize, replacement: &str) -> Edit {
        Edit {
            region: RegionRef::Body,
            flat_start,
            flat_end,
            replacement: replacement.to_string(),
        }
    }

    fn underline_body_edit(flat_start: usize, flat_end: usize, replacement: &str) -> UnderlineEdit {
        UnderlineEdit {
            region: RegionRef::Body,
            flat_start,
            flat_end,
            replacement: replacement.to_string(),
        }
    }

    #[test]
    fn replaces_a_whole_run() {
        let xml = wrap(r#"<w:p><w:r><w:t>John Smith</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let edited = apply_edits(&xml, &doc, &[body_edit(0, 10, "{{e.name}}")]).unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "{{e.name}}");
    }

    #[test]
    fn replaces_only_part_of_a_run_leaving_the_rest_intact() {
        let xml = wrap(r#"<w:p><w:r><w:t>Unit/Space number 204</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let edited = apply_edits(&xml, &doc, &[body_edit(18, 21, "{{u.num}}")]).unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "Unit/Space number {{u.num}}");
    }

    #[test]
    fn leaves_every_other_run_byte_identical() {
        let xml = wrap(
            r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Bold label: </w:t></w:r><w:r><w:t>204</w:t></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);
        let unit_run = doc.body.runs[1];

        let edited = apply_edits(
            &xml,
            &doc,
            &[body_edit(
                unit_run.flat_start,
                unit_run.flat_end,
                "{{u.num}}",
            )],
        )
        .unwrap();

        // The first run's own XML, including its bold formatting, must
        // survive completely untouched.
        assert!(edited.contains(r#"<w:rPr><w:b/></w:rPr><w:t>Bold label: </w:t>"#));
    }

    #[test]
    fn escapes_special_characters_in_the_replacement() {
        let xml = wrap(r#"<w:p><w:r><w:t>PLACEHOLDER</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let edited =
            apply_edits(&xml, &doc, &[body_edit(0, 11, "Smith & Sons <Storage>")]).unwrap();

        assert!(edited.contains("Smith &amp; Sons &lt;Storage&gt;"));
        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "Smith & Sons <Storage>");
    }

    #[test]
    fn splices_an_edit_spanning_two_runs() {
        // "John Smith" straddles both runs -- the first run keeps its
        // own lead-in ("Tenant: "), gets the full replacement appended,
        // and the second run's touched portion (all of it, here) is
        // emptied rather than refused.
        let xml =
            wrap(r#"<w:p><w:r><w:t>Tenant: John</w:t></w:r><w:r><w:t> Smith</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let edited = apply_edits(&xml, &doc, &[body_edit(8, 18, "{{e.name}}")]).unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "Tenant: {{e.name}}");
    }

    #[test]
    fn splices_an_edit_spanning_three_runs_leaving_each_ones_untouched_edges_intact() {
        // Mirrors the real bug this generalizes from: a blank's
        // underscore run split into three separate <w:t> elements by
        // Word for no visible reason (Sumas Mini Storage's real
        // "UNIT #__________________" + "__" + "_"). The label's own
        // text, in the first run, must survive; the fully-consumed
        // middle run and the touched tail of the last run must both end
        // up empty; nothing outside the touched runs may move.
        let xml = wrap(
            r#"<w:p><w:r><w:t>UNIT #____</w:t></w:r><w:r><w:t>__</w:t></w:r><w:r><w:t>_ (initial)</w:t></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);
        let blank_start = doc.body.text.find("____").unwrap();
        let blank_end = doc.body.text.find(" (initial)").unwrap();

        let edited = apply_edits(
            &xml,
            &doc,
            &[body_edit(blank_start, blank_end, "{{u.num}}")],
        )
        .unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "UNIT #{{u.num}} (initial)");
    }

    #[test]
    fn a_zero_width_insert_lands_before_a_blank_split_across_runs() {
        // The "preserve the blank" mode: insert only, nothing removed.
        // The underscores after the insertion point are themselves
        // split across two runs (same shape as the real Sumas bug), but
        // a zero-width edit never needs to touch them at all -- it
        // resolves to the label's own run, which is untouched by the
        // split.
        let xml = wrap(r#"<w:p><w:r><w:t>UNIT #</w:t></w:r><w:r><w:t>____</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        let insert_at = doc.body.text.find("____").unwrap();

        let edited =
            apply_edits(&xml, &doc, &[body_edit(insert_at, insert_at, "{{u.num}}")]).unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "UNIT #{{u.num}}____");
    }

    #[test]
    fn refuses_an_edit_touching_no_run_at_all() {
        let xml = wrap(r#"<w:p><w:r><w:t>John</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let result = apply_edits(&xml, &doc, &[body_edit(100, 110, "{{e.name}}")]);

        assert_eq!(
            result,
            Err(EditError::NoMatchingRun {
                region: RegionRef::Body,
                flat_start: 100,
                flat_end: 110
            })
        );
    }

    #[test]
    fn refuses_overlapping_edits_in_the_same_region() {
        let xml = wrap(r#"<w:p><w:r><w:t>John Smith</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let result = apply_edits(&xml, &doc, &[body_edit(0, 5, "A"), body_edit(3, 10, "B")]);

        assert_eq!(
            result,
            Err(EditError::OverlappingEdits {
                region: RegionRef::Body,
                first: (0, 5),
                second: (3, 10)
            })
        );
    }

    #[test]
    fn applies_two_non_overlapping_edits_in_the_same_run() {
        let xml = wrap(r#"<w:p><w:r><w:t>AAA BBB CCC</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let edited =
            apply_edits(&xml, &doc, &[body_edit(0, 3, "X"), body_edit(8, 11, "Z")]).unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "X BBB Z");
    }

    #[test]
    fn applies_edits_to_both_the_body_and_a_table_cell_in_one_call() {
        let xml = wrap(
            r#"<w:p><w:r><w:t>Before</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        );
        let doc = extract_flat_text(&xml);

        let edited = apply_edits(
            &xml,
            &doc,
            &[
                body_edit(0, 6, "{{f.name}}"),
                Edit {
                    region: RegionRef::TableCell(0),
                    flat_start: 0,
                    flat_end: 5,
                    replacement: "{{u.num}}".to_string(),
                },
            ],
        )
        .unwrap();

        let reflattened = extract_flat_text(&edited);
        assert_eq!(reflattened.body.text, "{{f.name}}");
        assert_eq!(reflattened.table_cells[0].text, "{{u.num}}");
    }

    #[test]
    fn two_edits_with_the_same_coordinates_in_different_regions_do_not_conflict() {
        // Body's flat_start:0 and the cell's flat_start:0 address
        // completely different physical text -- this must not be
        // rejected as an overlap.
        let xml = wrap(
            r#"<w:p><w:r><w:t>Before</w:t></w:r></w:p><w:tbl><w:tr><w:tc><w:p><w:r><w:t>Value</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        );
        let doc = extract_flat_text(&xml);

        let result = apply_edits(
            &xml,
            &doc,
            &[
                body_edit(0, 6, "{{f.name}}"),
                Edit {
                    region: RegionRef::TableCell(0),
                    flat_start: 0,
                    flat_end: 5,
                    replacement: "{{u.num}}".to_string(),
                },
            ],
        );

        assert!(result.is_ok());
    }

    #[test]
    fn underline_edit_deletes_a_pure_underscore_run_and_underlines_the_tag() {
        let xml = wrap(
            r#"<w:p><w:r><w:t>Move-In Date: ______________________________</w:t></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);
        let blank_start = doc
            .body
            .text
            .find("______________________________")
            .unwrap();
        let blank_end = blank_start + 30;

        let edited = apply_underline_edits(
            &xml,
            &doc,
            &[underline_body_edit(blank_start, blank_end, "{{m.indate}}")],
        )
        .unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "Move-In Date: {{m.indate}}");
        assert!(edited.contains("<w:t>Move-In Date: </w:t>"));
        assert!(edited.contains(r#"<w:rPr><w:u w:val="single"/></w:rPr><w:t>{{m.indate}}</w:t>"#));
    }

    #[test]
    fn underline_edit_only_underlines_the_blank_sharing_a_run_with_its_label() {
        // The real Sumas Mini Storage shape: the label and the blank's
        // first underscore share one run ("DATE: _"), with the rest of
        // the blank continuing in a second run. The label text must
        // keep its original (non-underlined) formatting even though
        // it's in the very same run as text that needs replacing.
        let xml = wrap(
            r#"<w:p><w:r><w:t>DATE: _</w:t></w:r><w:r><w:t>____________________</w:t></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);
        assert_eq!(doc.body.text, "DATE: _____________________"); // "DATE: " + 21 underscores

        let blank_start = "DATE: ".len();
        let blank_end = blank_start + 21;

        let edited = apply_underline_edits(
            &xml,
            &doc,
            &[underline_body_edit(blank_start, blank_end, "{{d.now}}")],
        )
        .unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "DATE: {{d.now}}");

        // The label survives as its own untouched, non-underlined run.
        assert!(edited.contains("<w:t>DATE: </w:t>"));
        assert!(edited.contains(r#"<w:rPr><w:u w:val="single"/></w:rPr><w:t>{{d.now}}</w:t>"#));
    }

    #[test]
    fn underline_edit_clones_the_runs_existing_formatting_onto_the_underlined_fragment() {
        let xml = wrap(
            r#"<w:p><w:r><w:rPr><w:sz w:val="20"/></w:rPr><w:t>SIZE__________</w:t></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);
        let blank_start = doc.body.text.find("__________").unwrap();
        let blank_end = blank_start + 10;

        let edited = apply_underline_edits(
            &xml,
            &doc,
            &[underline_body_edit(blank_start, blank_end, "{{u.dim}}")],
        )
        .unwrap();

        // Both the original size formatting AND the new underline must
        // be present on the underlined fragment.
        assert!(edited.contains(r#"<w:rPr><w:sz w:val="20"/><w:u w:val="single"/></w:rPr>"#));
    }

    #[test]
    fn underline_edit_overrides_rather_than_duplicates_an_existing_underline() {
        let xml =
            wrap(r#"<w:p><w:r><w:rPr><w:u w:val="double"/></w:rPr><w:t>____</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let edited =
            apply_underline_edits(&xml, &doc, &[underline_body_edit(0, 4, "{{x}}")]).unwrap();

        assert!(edited.contains(r#"<w:u w:val="single"/>"#));
        assert!(!edited.contains("double"));
        assert_eq!(edited.matches("<w:u ").count(), 1);
    }

    #[test]
    fn underline_edit_inserts_a_fresh_rpr_for_a_run_with_no_formatting_at_all() {
        let xml = wrap(r#"<w:p><w:r><w:t>____</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let edited =
            apply_underline_edits(&xml, &doc, &[underline_body_edit(0, 4, "{{x}}")]).unwrap();

        assert!(
            edited.contains(r#"<w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t>{{x}}</w:t></w:r>"#)
        );
    }

    #[test]
    fn underline_edit_leaves_an_unrelated_sibling_run_byte_identical() {
        let xml = wrap(
            r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Bold label: </w:t></w:r><w:r><w:t>____</w:t></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);
        let blank_run = doc.body.runs[1];

        let edited = apply_underline_edits(
            &xml,
            &doc,
            &[underline_body_edit(
                blank_run.flat_start,
                blank_run.flat_end,
                "{{x}}",
            )],
        )
        .unwrap();

        assert!(edited.contains(r#"<w:rPr><w:b/></w:rPr><w:t>Bold label: </w:t>"#));
    }

    #[test]
    fn underline_edit_refuses_an_edit_touching_no_run_at_all() {
        let xml = wrap(r#"<w:p><w:r><w:t>John</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let result = apply_underline_edits(&xml, &doc, &[underline_body_edit(100, 110, "x")]);

        assert_eq!(
            result,
            Err(EditError::NoMatchingRun {
                region: RegionRef::Body,
                flat_start: 100,
                flat_end: 110
            })
        );
    }

    #[test]
    fn underline_edit_refuses_overlapping_edits_in_the_same_region() {
        let xml = wrap(r#"<w:p><w:r><w:t>__________</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let result = apply_underline_edits(
            &xml,
            &doc,
            &[
                underline_body_edit(0, 5, "A"),
                underline_body_edit(3, 10, "B"),
            ],
        );

        assert_eq!(
            result,
            Err(EditError::OverlappingEdits {
                region: RegionRef::Body,
                first: (0, 5),
                second: (3, 10)
            })
        );
    }

    #[test]
    fn apply_all_edits_combines_a_plain_edit_and_an_underline_edit_in_one_pass() {
        // A plain value substitution (detect_candidates-style) and an
        // underline substitution (recognize_blanks-style) both landing
        // in the same `apply` call, on different runs -- must both take
        // effect correctly without either's byte offsets going stale
        // from the other's edit.
        let xml = wrap(
            r#"<w:p><w:r><w:t>Tenant: Atherton Storage</w:t></w:r></w:p><w:p><w:r><w:t>SIZE_____________</w:t></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);

        let value_start = doc.body.text.find("Atherton Storage").unwrap();
        let value_end = value_start + "Atherton Storage".len();
        let blank_start = doc.body.text.find("_____________").unwrap();
        let blank_end = blank_start + 13;

        let edited = apply_all_edits(
            &xml,
            &doc,
            &[body_edit(value_start, value_end, "{{f.name}}")],
            &[underline_body_edit(blank_start, blank_end, "{{u.dim}}")],
        )
        .unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "Tenant: {{f.name}}\nSIZE{{u.dim}}");
        assert!(edited.contains(r#"<w:rPr><w:u w:val="single"/></w:rPr><w:t>{{u.dim}}</w:t>"#));
    }

    #[test]
    fn apply_all_edits_handles_a_plain_edit_and_an_underline_edit_sharing_one_run() {
        // Real documents routinely put several labels and blanks in one
        // physical run -- Word has no reason to break a run just
        // because two different substitutions will eventually land in
        // it. A plain edit only ever replaces a run's own <w:t> content
        // range; an underline edit replaces the run's *whole* element.
        // Computed independently against the same run, those two
        // replacement ranges would overlap and corrupt the splice --
        // this reproduces the exact shape that panicked before the two
        // kinds were unified into one per-run reconstruction.
        let xml = wrap(r#"<w:p><w:r><w:t>AAA_____BBB_______________CCC</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        assert_eq!(doc.body.runs.len(), 1, "test setup must be a single run");

        let edited = apply_all_edits(
            &xml,
            &doc,
            &[body_edit(3, 8, "{{a}}")],
            &[underline_body_edit(11, 26, "{{b}}")],
        )
        .unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "AAA{{a}}BBB{{b}}CCC");
        assert!(edited.contains(r#"<w:rPr><w:u w:val="single"/></w:rPr><w:t>{{b}}</w:t>"#));
    }
}
