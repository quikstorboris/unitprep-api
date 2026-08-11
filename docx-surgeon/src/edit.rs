use crate::read::{FlatDocument, RegionRef};

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
    /// The edit's range doesn't fall entirely inside one `<w:t>` run --
    /// refused rather than guessed at. Splicing across a run boundary
    /// would mean editing two separate XML elements for what the flat
    /// text shows as one contiguous span, which this crate's whole
    /// safety model (touch only the exact bytes of a run being
    /// replaced) isn't built to reason about correctly.
    SpansMultipleRuns {
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
/// Each edit must fall entirely within a single run's decoded text --
/// see [`EditError::SpansMultipleRuns`]. A run touched by one or more
/// edits always has its *entire* raw XML text content replaced
/// wholesale (decode -> splice in decoded space -> re-encode -> replace
/// the whole `<w:t>...</w:t>` content), even when an edit only touches
/// part of that run's text. This is deliberate, not a shortcut: slicing
/// raw XML bytes mid-entity (`&amp;` is 5 bytes, decodes to 1) would
/// silently corrupt the document the moment a run's text contains one.
/// Replacing the whole run's content atomically after a decode/re-
/// encode round trip makes that failure mode impossible by
/// construction, at the cost of only ever touching whole runs, never
/// partial byte ranges within one.
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
    check_no_overlaps(edits)?;

    // Group edits by the (region, run) they land in, since more than
    // one edit can legitimately target the same run -- and a run index
    // is only unique *within* its own region, never across regions.
    let mut by_run: Vec<(RegionRef, usize, Vec<&Edit>)> = Vec::new();
    for edit in edits {
        let flat = doc.region(edit.region);
        let run = flat.run_containing(edit.flat_start, edit.flat_end).ok_or(
            EditError::SpansMultipleRuns {
                region: edit.region,
                flat_start: edit.flat_start,
                flat_end: edit.flat_end,
            },
        )?;
        let run_index = flat
            .runs
            .iter()
            .position(|r| r.flat_start == run.flat_start && r.flat_end == run.flat_end)
            .expect("run_containing returned a run that isn't in this region's runs");

        match by_run
            .iter_mut()
            .find(|(region, idx, _)| *region == edit.region && *idx == run_index)
        {
            Some((_, _, group)) => group.push(edit),
            None => by_run.push((edit.region, run_index, vec![edit])),
        }
    }

    // For each affected run, splice its edits into the run's own
    // decoded text (local coordinates), then re-encode the whole thing.
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    for (region, run_index, mut group) in by_run {
        let flat = doc.region(region);
        let run = flat.runs[run_index];
        group.sort_by_key(|e| e.flat_start);

        let original = &flat.text[run.flat_start..run.flat_end];
        let mut rebuilt = String::new();
        let mut cursor = 0usize;
        for edit in group {
            let local_start = edit.flat_start - run.flat_start;
            let local_end = edit.flat_end - run.flat_start;
            rebuilt.push_str(&original[cursor..local_start]);
            rebuilt.push_str(&edit.replacement);
            cursor = local_end;
        }
        rebuilt.push_str(&original[cursor..]);

        replacements.push((
            run.xml_content_start,
            run.xml_content_end,
            xml_escape_text(&rebuilt),
        ));
    }

    replacements.sort_by_key(|(start, _, _)| *start);

    let mut result = String::with_capacity(document_xml.len());
    let mut cursor = 0usize;
    for (start, end, new_content) in replacements {
        result.push_str(&document_xml[cursor..start]);
        result.push_str(&new_content);
        cursor = end;
    }
    result.push_str(&document_xml[cursor..]);

    Ok(result)
}

/// Overlap is only meaningful *within* one region -- two edits in
/// different regions can share identical numeric coordinates without
/// conflicting at all, since those coordinates are relative to
/// different text entirely.
fn check_no_overlaps(edits: &[Edit]) -> Result<(), EditError> {
    let mut seen_regions: Vec<RegionRef> = Vec::new();
    for edit in edits {
        if !seen_regions.contains(&edit.region) {
            seen_regions.push(edit.region);
        }
    }

    for region in seen_regions {
        let mut same_region: Vec<&Edit> = edits.iter().filter(|e| e.region == region).collect();
        same_region.sort_by_key(|e| e.flat_start);
        for pair in same_region.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if b.flat_start < a.flat_end {
                return Err(EditError::OverlappingEdits {
                    region,
                    first: (a.flat_start, a.flat_end),
                    second: (b.flat_start, b.flat_end),
                });
            }
        }
    }
    Ok(())
}

fn xml_escape_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(c),
        }
    }
    escaped
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
    fn refuses_an_edit_spanning_two_runs() {
        let xml =
            wrap(r#"<w:p><w:r><w:t>Tenant: John</w:t></w:r><w:r><w:t> Smith</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let result = apply_edits(&xml, &doc, &[body_edit(8, 18, "{{e.name}}")]);

        assert_eq!(
            result,
            Err(EditError::SpansMultipleRuns {
                region: RegionRef::Body,
                flat_start: 8,
                flat_end: 18
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
}
