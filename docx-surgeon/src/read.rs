use quick_xml::events::Event;
use quick_xml::Reader;

/// One `<w:t>` element's contribution to the flattened text.
///
/// `xml_content_start`/`xml_content_end` is the byte range in the raw
/// `document.xml`, strictly between this element's opening and closing
/// tags -- the exact bytes to overwrite for an edit touching this run.
/// This range is never sliced mid-entity: [`crate::edit::apply_edits`]
/// always replaces this whole range, even when an edit only touches
/// part of the run's text, specifically to avoid the byte-length
/// mismatch between an entity's encoded form (`&amp;`) and its decoded
/// form (`&`) that byte-precise slicing inside encoded text would risk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunSpan {
    pub flat_start: usize,
    pub flat_end: usize,
    pub xml_content_start: usize,
    pub xml_content_end: usize,
    pub is_underlined: bool,
}

/// The flattened plain text of a document body, plus enough of a map
/// back to `document.xml` to make a surgical edit possible.
#[derive(Debug, Clone)]
pub struct FlatText {
    pub text: String,
    /// Every `<w:t>` run's contribution, in document order, non-
    /// overlapping and sorted by `flat_start`.
    pub runs: Vec<RunSpan>,
}

impl FlatText {
    /// The single run entirely containing `[flat_start, flat_end)`, if
    /// one exists. `None` if the range spans more than one run (or
    /// none) -- see the edit module for why that case is refused
    /// rather than guessed at.
    pub fn run_containing(&self, flat_start: usize, flat_end: usize) -> Option<&RunSpan> {
        self.runs
            .iter()
            .find(|run| run.flat_start <= flat_start && flat_end <= run.flat_end)
    }
}

/// Walks a `word/document.xml` body and flattens every `<w:t>` run's
/// text into one string, in document order, recording where each run's
/// text came from so an edit can be spliced back into the original
/// bytes later without touching anything else in the file.
///
/// A `\n` is inserted between paragraphs and a `\t` for every
/// `<w:tab/>` -- both contribute to the flat text for positional and
/// contextual accuracy (so a pattern search doesn't see two paragraphs
/// as one run-on sentence), but neither is a [`RunSpan`]: only real
/// `<w:t>` content is addressable by an edit. Table cells are walked
/// like any other paragraph container -- this function has no notion
/// of table structure at all; that's a concern for whatever recognizer
/// consumes this flat text, not for extraction itself.
///
/// Does not attempt to represent every OOXML feature (content controls,
/// tracked changes, comments, fields). A document using those will
/// still flatten to readable text, just without special handling for
/// them -- acceptable for this crate's scope, since it never needs to
/// understand what a run *means*, only where its text physically is.
pub fn extract_flat_text(document_xml: &str) -> FlatText {
    let mut reader = Reader::from_str(document_xml);

    let mut text = String::new();
    let mut runs = Vec::new();

    let mut in_run_props = false;
    let mut current_run_underlined = false;
    let mut wrote_any_paragraph = false;

    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Err(_) => break,

            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"p" => {
                    if wrote_any_paragraph {
                        text.push('\n');
                    }
                    wrote_any_paragraph = true;
                }
                b"r" => {
                    current_run_underlined = false;
                }
                b"rPr" => {
                    in_run_props = true;
                }
                b"t" => {
                    let content_start = reader.buffer_position() as usize;
                    match reader.read_event() {
                        Ok(Event::Text(t)) => {
                            let content_end = reader.buffer_position() as usize;
                            let decoded = t.unescape().map(|s| s.into_owned()).unwrap_or_default();
                            push_run(
                                &mut text,
                                &mut runs,
                                &decoded,
                                content_start,
                                content_end,
                                current_run_underlined,
                            );
                        }
                        Ok(Event::End(_)) => {
                            // Empty <w:t></w:t> -- zero-length run, still recorded.
                            push_run(
                                &mut text,
                                &mut runs,
                                "",
                                content_start,
                                content_start,
                                current_run_underlined,
                            );
                        }
                        _ => {}
                    }
                }
                _ => {}
            },

            Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"tab" => text.push('\t'),
                b"u" if in_run_props => {
                    let explicitly_none = e.attributes().flatten().any(|a| {
                        a.key.local_name().as_ref() == b"val" && a.value.as_ref() == b"none"
                    });
                    current_run_underlined = !explicitly_none;
                }
                b"t" => {
                    // Self-closing <w:t/> -- zero-length run.
                    let pos = reader.buffer_position() as usize;
                    push_run(&mut text, &mut runs, "", pos, pos, current_run_underlined);
                }
                _ => {}
            },

            Ok(Event::End(e)) if e.local_name().as_ref() == b"rPr" => {
                in_run_props = false;
            }

            _ => {}
        }
    }

    FlatText { text, runs }
}

fn push_run(
    text: &mut String,
    runs: &mut Vec<RunSpan>,
    decoded: &str,
    xml_content_start: usize,
    xml_content_end: usize,
    is_underlined: bool,
) {
    let flat_start = text.len();
    text.push_str(decoded);
    let flat_end = text.len();
    runs.push(RunSpan {
        flat_start,
        flat_end,
        xml_content_start,
        xml_content_end,
        is_underlined,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap(body: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#
        )
    }

    #[test]
    fn flattens_a_single_run() {
        let xml = wrap(r#"<w:p><w:r><w:t>Hello</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml);

        assert_eq!(flat.text, "Hello");
        assert_eq!(flat.runs.len(), 1);
        assert_eq!(flat.runs[0].flat_start, 0);
        assert_eq!(flat.runs[0].flat_end, 5);
    }

    #[test]
    fn joins_multiple_runs_in_one_paragraph_with_no_separator() {
        let xml =
            wrap(r#"<w:p><w:r><w:t>Tenant: </w:t></w:r><w:r><w:t>John Smith</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml);

        assert_eq!(flat.text, "Tenant: John Smith");
        assert_eq!(flat.runs.len(), 2);
    }

    #[test]
    fn separates_paragraphs_with_a_newline() {
        let xml = wrap(
            r#"<w:p><w:r><w:t>First</w:t></w:r></w:p><w:p><w:r><w:t>Second</w:t></w:r></w:p>"#,
        );
        let flat = extract_flat_text(&xml);

        assert_eq!(flat.text, "First\nSecond");
    }

    #[test]
    fn represents_a_tab_as_a_tab_character_with_no_run() {
        let xml = wrap(
            r#"<w:p><w:r><w:t>Name:</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>Jane</w:t></w:r></w:p>"#,
        );
        let flat = extract_flat_text(&xml);

        assert_eq!(flat.text, "Name:\tJane");
        assert_eq!(flat.runs.len(), 2);
    }

    #[test]
    fn decodes_xml_entities_in_run_text() {
        let xml = wrap(r#"<w:p><w:r><w:t>Smith &amp; Sons</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml);

        assert_eq!(flat.text, "Smith & Sons");
    }

    #[test]
    fn detects_an_underlined_run() {
        let xml =
            wrap(r#"<w:p><w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t>   </w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml);

        assert!(flat.runs[0].is_underlined);
    }

    #[test]
    fn does_not_treat_explicit_underline_none_as_underlined() {
        let xml =
            wrap(r#"<w:p><w:r><w:rPr><w:u w:val="none"/></w:rPr><w:t>plain</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml);

        assert!(!flat.runs[0].is_underlined);
    }

    #[test]
    fn records_a_zero_length_run_for_an_empty_t_element() {
        let xml = wrap(r#"<w:p><w:r><w:t></w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml);

        assert_eq!(flat.text, "");
        assert_eq!(flat.runs.len(), 1);
        assert_eq!(flat.runs[0].flat_start, flat.runs[0].flat_end);
    }

    #[test]
    fn records_a_zero_length_run_for_a_self_closing_t_element() {
        let xml = wrap(r#"<w:p><w:r><w:t/></w:r></w:p>"#);
        let flat = extract_flat_text(&xml);

        assert_eq!(flat.text, "");
        assert_eq!(flat.runs.len(), 1);
    }

    #[test]
    fn run_containing_finds_the_span_that_fully_covers_a_range() {
        let xml =
            wrap(r#"<w:p><w:r><w:t>Tenant: </w:t></w:r><w:r><w:t>John Smith</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml);

        let found = flat.run_containing(8, 18).unwrap();
        assert_eq!(found.flat_start, 8);
        assert_eq!(found.flat_end, 18);
    }

    #[test]
    fn run_containing_refuses_a_range_spanning_two_runs() {
        let xml =
            wrap(r#"<w:p><w:r><w:t>Tenant: John</w:t></w:r><w:r><w:t> Smith</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml);

        // "John Smith" straddles both runs -- must not silently pick one.
        assert!(flat.run_containing(8, 18).is_none());
    }
}
