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
///
/// Always an absolute offset into the *original* `document.xml`,
/// regardless of which [`FlatText`] region (the document body, or one
/// particular table cell) the run was collected into -- a region only
/// changes what `flat_start`/`flat_end` are relative to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunSpan {
    pub flat_start: usize,
    pub flat_end: usize,
    pub xml_content_start: usize,
    pub xml_content_end: usize,
    pub is_underlined: bool,
}

/// The flattened plain text of one addressable region, plus enough of a
/// map back to `document.xml` to make a surgical edit possible.
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

/// A `document.xml` body, flattened into independently-addressable text
/// regions rather than one run-on string.
///
/// `body` is the document's own paragraph flow -- everything *not*
/// inside a table cell, in document order (a paragraph before a table
/// and a paragraph after it share this one region, joined by `\n` same
/// as any other two paragraphs). `table_cells` holds one entry per
/// `<w:tc>`, in document order, each flattened exactly like a
/// standalone mini-document: its own paragraphs joined by `\n`, its own
/// [`RunSpan`]s addressed relative to its own text.
///
/// Splitting cells out this way -- rather than flattening a whole row,
/// or the whole table, into one string -- is deliberate: two adjacent
/// cells' text must never be able to read as one run-on sentence (a
/// label in one cell being mistaken for context belonging to the value
/// in the next), and a pattern search inside one cell must never match
/// across a cell boundary it can't see. Nesting (a table inside a table
/// cell) is not a case this has been designed or tested against, but
/// degrades reasonably: an inner `<w:tc>` still becomes its own region,
/// same as any other.
#[derive(Debug, Clone)]
pub struct FlatDocument {
    pub body: FlatText,
    pub table_cells: Vec<FlatText>,
}

impl FlatDocument {
    /// The region `r` identifies. Panics if `r` is a `TableCell` index
    /// out of range for this document -- same "caller-guaranteed valid"
    /// contract as `Vec`'s own indexing, since a [`RegionRef`] is only
    /// ever meaningful paired with the specific `FlatDocument` it was
    /// produced from.
    pub fn region(&self, r: RegionRef) -> &FlatText {
        match r {
            RegionRef::Body => &self.body,
            RegionRef::TableCell(i) => &self.table_cells[i],
        }
    }
}

/// Identifies which region of a [`FlatDocument`] a set of flat-text
/// coordinates (a [`RunSpan`], an edit) is relative to. Two regions can
/// both have a run at `flat_start: 0` -- it is never meaningful to
/// compare flat coordinates across regions without knowing which region
/// each side belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionRef {
    Body,
    TableCell(usize),
}

/// One region's in-progress accumulator while walking the document --
/// either the shared `body` accumulator, or one pushed per `<w:tc>`.
struct Accumulator {
    text: String,
    runs: Vec<RunSpan>,
    wrote_any_paragraph: bool,
}

impl Accumulator {
    fn new() -> Self {
        Accumulator {
            text: String::new(),
            runs: Vec::new(),
            wrote_any_paragraph: false,
        }
    }

    fn finish(self) -> FlatText {
        FlatText {
            text: self.text,
            runs: self.runs,
        }
    }
}

/// Walks a `word/document.xml` body and flattens every `<w:t>` run's
/// text, in document order, recording where each run's text came from
/// so an edit can be spliced back into the original bytes later without
/// touching anything else in the file.
///
/// A `\n` is inserted between paragraphs (within the same region) and a
/// `\t` for every `<w:tab/>` -- both contribute to the flat text for
/// positional and contextual accuracy, but neither is a [`RunSpan`]:
/// only real `<w:t>` content is addressable by an edit. Text inside a
/// `<w:tc>` (table cell) is routed into its own entry in
/// [`FlatDocument::table_cells`] rather than the shared `body` region --
/// see that struct's doc comment for why.
///
/// Does not attempt to represent every OOXML feature (content controls,
/// tracked changes, comments, fields). A document using those will
/// still flatten to readable text, just without special handling for
/// them -- acceptable for this crate's scope, since it never needs to
/// understand what a run *means*, only where its text physically is.
pub fn extract_flat_text(document_xml: &str) -> FlatDocument {
    let mut reader = Reader::from_str(document_xml);

    let mut body = Accumulator::new();
    let mut cell_stack: Vec<Accumulator> = Vec::new();
    let mut table_cells = Vec::new();

    let mut in_run_props = false;
    let mut current_run_underlined = false;

    loop {
        match reader.read_event() {
            Ok(Event::Eof) => break,
            Err(_) => break,

            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"tc" => {
                    cell_stack.push(Accumulator::new());
                }
                b"p" => {
                    let acc = current_acc(&mut cell_stack, &mut body);
                    if acc.wrote_any_paragraph {
                        acc.text.push('\n');
                    }
                    acc.wrote_any_paragraph = true;
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
                                current_acc(&mut cell_stack, &mut body),
                                &decoded,
                                content_start,
                                content_end,
                                current_run_underlined,
                            );
                        }
                        Ok(Event::End(_)) => {
                            // Empty <w:t></w:t> -- zero-length run, still recorded.
                            push_run(
                                current_acc(&mut cell_stack, &mut body),
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
                b"tab" => current_acc(&mut cell_stack, &mut body).text.push('\t'),
                b"u" if in_run_props => {
                    let explicitly_none = e.attributes().flatten().any(|a| {
                        a.key.local_name().as_ref() == b"val" && a.value.as_ref() == b"none"
                    });
                    current_run_underlined = !explicitly_none;
                }
                b"t" => {
                    // Self-closing <w:t/> -- zero-length run.
                    let pos = reader.buffer_position() as usize;
                    push_run(
                        current_acc(&mut cell_stack, &mut body),
                        "",
                        pos,
                        pos,
                        current_run_underlined,
                    );
                }
                _ => {}
            },

            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"rPr" => in_run_props = false,
                b"tc" => {
                    if let Some(acc) = cell_stack.pop() {
                        table_cells.push(acc.finish());
                    }
                }
                _ => {}
            },

            _ => {}
        }
    }

    FlatDocument {
        body: body.finish(),
        table_cells,
    }
}

/// The accumulator text/runs currently being written to: the innermost
/// open `<w:tc>`, if any, otherwise the shared body region.
fn current_acc<'a>(
    cell_stack: &'a mut [Accumulator],
    body: &'a mut Accumulator,
) -> &'a mut Accumulator {
    cell_stack.last_mut().unwrap_or(body)
}

fn push_run(
    acc: &mut Accumulator,
    decoded: &str,
    xml_content_start: usize,
    xml_content_end: usize,
    is_underlined: bool,
) {
    let flat_start = acc.text.len();
    acc.text.push_str(decoded);
    let flat_end = acc.text.len();
    acc.runs.push(RunSpan {
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
        let flat = extract_flat_text(&xml).body;

        assert_eq!(flat.text, "Hello");
        assert_eq!(flat.runs.len(), 1);
        assert_eq!(flat.runs[0].flat_start, 0);
        assert_eq!(flat.runs[0].flat_end, 5);
    }

    #[test]
    fn joins_multiple_runs_in_one_paragraph_with_no_separator() {
        let xml =
            wrap(r#"<w:p><w:r><w:t>Tenant: </w:t></w:r><w:r><w:t>John Smith</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        assert_eq!(flat.text, "Tenant: John Smith");
        assert_eq!(flat.runs.len(), 2);
    }

    #[test]
    fn separates_paragraphs_with_a_newline() {
        let xml = wrap(
            r#"<w:p><w:r><w:t>First</w:t></w:r></w:p><w:p><w:r><w:t>Second</w:t></w:r></w:p>"#,
        );
        let flat = extract_flat_text(&xml).body;

        assert_eq!(flat.text, "First\nSecond");
    }

    #[test]
    fn represents_a_tab_as_a_tab_character_with_no_run() {
        let xml = wrap(
            r#"<w:p><w:r><w:t>Name:</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>Jane</w:t></w:r></w:p>"#,
        );
        let flat = extract_flat_text(&xml).body;

        assert_eq!(flat.text, "Name:\tJane");
        assert_eq!(flat.runs.len(), 2);
    }

    #[test]
    fn decodes_xml_entities_in_run_text() {
        let xml = wrap(r#"<w:p><w:r><w:t>Smith &amp; Sons</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        assert_eq!(flat.text, "Smith & Sons");
    }

    #[test]
    fn detects_an_underlined_run() {
        let xml =
            wrap(r#"<w:p><w:r><w:rPr><w:u w:val="single"/></w:rPr><w:t>   </w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        assert!(flat.runs[0].is_underlined);
    }

    #[test]
    fn does_not_treat_explicit_underline_none_as_underlined() {
        let xml =
            wrap(r#"<w:p><w:r><w:rPr><w:u w:val="none"/></w:rPr><w:t>plain</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        assert!(!flat.runs[0].is_underlined);
    }

    #[test]
    fn records_a_zero_length_run_for_an_empty_t_element() {
        let xml = wrap(r#"<w:p><w:r><w:t></w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        assert_eq!(flat.text, "");
        assert_eq!(flat.runs.len(), 1);
        assert_eq!(flat.runs[0].flat_start, flat.runs[0].flat_end);
    }

    #[test]
    fn records_a_zero_length_run_for_a_self_closing_t_element() {
        let xml = wrap(r#"<w:p><w:r><w:t/></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        assert_eq!(flat.text, "");
        assert_eq!(flat.runs.len(), 1);
    }

    #[test]
    fn run_containing_finds_the_span_that_fully_covers_a_range() {
        let xml =
            wrap(r#"<w:p><w:r><w:t>Tenant: </w:t></w:r><w:r><w:t>John Smith</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        let found = flat.run_containing(8, 18).unwrap();
        assert_eq!(found.flat_start, 8);
        assert_eq!(found.flat_end, 18);
    }

    #[test]
    fn run_containing_refuses_a_range_spanning_two_runs() {
        let xml =
            wrap(r#"<w:p><w:r><w:t>Tenant: John</w:t></w:r><w:r><w:t> Smith</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        // "John Smith" straddles both runs -- must not silently pick one.
        assert!(flat.run_containing(8, 18).is_none());
    }

    fn cell(text: &str) -> String {
        format!(r#"<w:tc><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:tc>"#)
    }

    #[test]
    fn each_table_cell_becomes_its_own_region() {
        let xml = wrap(&format!(
            r#"<w:tbl><w:tr>{}{}</w:tr></w:tbl>"#,
            cell("Move-In Date"),
            cell("02/01/2026")
        ));
        let doc = extract_flat_text(&xml);

        assert_eq!(doc.table_cells.len(), 2);
        assert_eq!(doc.table_cells[0].text, "Move-In Date");
        assert_eq!(doc.table_cells[1].text, "02/01/2026");
    }

    #[test]
    fn adjacent_cells_never_read_as_one_run_on_string() {
        // The whole point of per-cell regions: a label in one cell must
        // never be able to match "close to" a value in the next, because
        // there is no shared coordinate space for a search to see both.
        let xml = wrap(&format!(
            r#"<w:tbl><w:tr>{}{}</w:tr></w:tbl>"#,
            cell("Label"),
            cell("Value")
        ));
        let doc = extract_flat_text(&xml);

        assert!(!doc.table_cells[0].text.contains("Value"));
        assert!(!doc.table_cells[1].text.contains("Label"));
    }

    #[test]
    fn table_cell_text_is_excluded_from_the_body_region() {
        let xml = wrap(&format!(
            r#"<w:p><w:r><w:t>Before</w:t></w:r></w:p><w:tbl><w:tr>{}</w:tr></w:tbl><w:p><w:r><w:t>After</w:t></w:r></w:p>"#,
            cell("Inside cell")
        ));
        let doc = extract_flat_text(&xml);

        assert_eq!(doc.body.text, "Before\nAfter");
        assert_eq!(doc.table_cells[0].text, "Inside cell");
    }

    #[test]
    fn a_cell_with_multiple_paragraphs_joins_them_with_a_newline() {
        let xml = wrap(
            r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>First line</w:t></w:r></w:p><w:p><w:r><w:t>Second line</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        );
        let doc = extract_flat_text(&xml);

        assert_eq!(doc.table_cells[0].text, "First line\nSecond line");
    }

    #[test]
    fn a_cells_run_spans_are_relative_to_its_own_region_not_the_body() {
        let xml = wrap(&format!(
            r#"<w:p><w:r><w:t>Before</w:t></w:r></w:p><w:tbl><w:tr>{}</w:tr></w:tbl>"#,
            cell("Value")
        ));
        let doc = extract_flat_text(&xml);

        // If the cell's run offsets had leaked the body's length in, this
        // would be 6 (len("Before")) instead of 0.
        assert_eq!(doc.table_cells[0].runs[0].flat_start, 0);
        assert_eq!(doc.table_cells[0].runs[0].flat_end, 5);
    }
}
