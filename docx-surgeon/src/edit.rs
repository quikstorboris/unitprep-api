use crate::read::FlatText;

/// One text replacement, expressed in flat-text coordinates (the same
/// coordinate space [`crate::read::extract_flat_text`] returns).
#[derive(Debug, Clone)]
pub struct Edit {
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
    SpansMultipleRuns { flat_start: usize, flat_end: usize },
    /// Two edits overlap in flat-text coordinates.
    OverlappingEdits {
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
pub fn apply_edits(
    document_xml: &str,
    flat: &FlatText,
    edits: &[Edit],
) -> Result<String, EditError> {
    check_no_overlaps(edits)?;

    // Group edits by the run they land in, since more than one edit
    // can legitimately target the same run.
    let mut by_run: Vec<(usize, Vec<&Edit>)> = Vec::new();
    for edit in edits {
        let run = flat.run_containing(edit.flat_start, edit.flat_end).ok_or(
            EditError::SpansMultipleRuns {
                flat_start: edit.flat_start,
                flat_end: edit.flat_end,
            },
        )?;
        let run_index = flat
            .runs
            .iter()
            .position(|r| r.flat_start == run.flat_start && r.flat_end == run.flat_end)
            .expect("run_containing returned a run that isn't in flat.runs");

        match by_run.iter_mut().find(|(idx, _)| *idx == run_index) {
            Some((_, group)) => group.push(edit),
            None => by_run.push((run_index, vec![edit])),
        }
    }

    // For each affected run, splice its edits into the run's own
    // decoded text (local coordinates), then re-encode the whole thing.
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    for (run_index, mut group) in by_run {
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

fn check_no_overlaps(edits: &[Edit]) -> Result<(), EditError> {
    let mut sorted: Vec<&Edit> = edits.iter().collect();
    sorted.sort_by_key(|e| e.flat_start);
    for pair in sorted.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if b.flat_start < a.flat_end {
            return Err(EditError::OverlappingEdits {
                first: (a.flat_start, a.flat_end),
                second: (b.flat_start, b.flat_end),
            });
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

    #[test]
    fn replaces_a_whole_run() {
        let xml = wrap(r#"<w:p><w:r><w:t>John Smith</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        let edited = apply_edits(
            &xml,
            &flat,
            &[Edit {
                flat_start: 0,
                flat_end: 10,
                replacement: "{{e.name}}".to_string(),
            }],
        )
        .unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "{{e.name}}");
    }

    #[test]
    fn replaces_only_part_of_a_run_leaving_the_rest_intact() {
        let xml = wrap(r#"<w:p><w:r><w:t>Unit/Space number 204</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        let edited = apply_edits(
            &xml,
            &flat,
            &[Edit {
                flat_start: 18,
                flat_end: 21,
                replacement: "{{u.num}}".to_string(),
            }],
        )
        .unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "Unit/Space number {{u.num}}");
    }

    #[test]
    fn leaves_every_other_run_byte_identical() {
        let xml = wrap(
            r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Bold label: </w:t></w:r><w:r><w:t>204</w:t></w:r></w:p>"#,
        );
        let flat = extract_flat_text(&xml).body;
        let unit_run = flat.runs[1];

        let edited = apply_edits(
            &xml,
            &flat,
            &[Edit {
                flat_start: unit_run.flat_start,
                flat_end: unit_run.flat_end,
                replacement: "{{u.num}}".to_string(),
            }],
        )
        .unwrap();

        // The first run's own XML, including its bold formatting, must
        // survive completely untouched.
        assert!(edited.contains(r#"<w:rPr><w:b/></w:rPr><w:t>Bold label: </w:t>"#));
    }

    #[test]
    fn escapes_special_characters_in_the_replacement() {
        let xml = wrap(r#"<w:p><w:r><w:t>PLACEHOLDER</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        let edited = apply_edits(
            &xml,
            &flat,
            &[Edit {
                flat_start: 0,
                flat_end: 11,
                replacement: "Smith & Sons <Storage>".to_string(),
            }],
        )
        .unwrap();

        assert!(edited.contains("Smith &amp; Sons &lt;Storage&gt;"));
        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "Smith & Sons <Storage>");
    }

    #[test]
    fn refuses_an_edit_spanning_two_runs() {
        let xml =
            wrap(r#"<w:p><w:r><w:t>Tenant: John</w:t></w:r><w:r><w:t> Smith</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        let result = apply_edits(
            &xml,
            &flat,
            &[Edit {
                flat_start: 8,
                flat_end: 18,
                replacement: "{{e.name}}".to_string(),
            }],
        );

        assert_eq!(
            result,
            Err(EditError::SpansMultipleRuns {
                flat_start: 8,
                flat_end: 18
            })
        );
    }

    #[test]
    fn refuses_overlapping_edits() {
        let xml = wrap(r#"<w:p><w:r><w:t>John Smith</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        let result = apply_edits(
            &xml,
            &flat,
            &[
                Edit {
                    flat_start: 0,
                    flat_end: 5,
                    replacement: "A".to_string(),
                },
                Edit {
                    flat_start: 3,
                    flat_end: 10,
                    replacement: "B".to_string(),
                },
            ],
        );

        assert_eq!(
            result,
            Err(EditError::OverlappingEdits {
                first: (0, 5),
                second: (3, 10)
            })
        );
    }

    #[test]
    fn applies_two_non_overlapping_edits_in_the_same_run() {
        let xml = wrap(r#"<w:p><w:r><w:t>AAA BBB CCC</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        let edited = apply_edits(
            &xml,
            &flat,
            &[
                Edit {
                    flat_start: 0,
                    flat_end: 3,
                    replacement: "X".to_string(),
                },
                Edit {
                    flat_start: 8,
                    flat_end: 11,
                    replacement: "Z".to_string(),
                },
            ],
        )
        .unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "X BBB Z");
    }
}
