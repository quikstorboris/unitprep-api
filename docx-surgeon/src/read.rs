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
    /// Byte offset of this run's own `<w:r` opening bracket -- the start
    /// of the *whole* run element, not just its `<w:t>` content. Needed
    /// to rebuild a run from scratch (splitting it into more than one
    /// `<w:r>`, e.g. [`crate::edit::apply_hidden_blank_edits`]) rather
    /// than just replacing its text content in place.
    pub run_start: usize,
    /// Byte offset immediately after this run's closing `</w:r>`.
    pub run_end: usize,
    /// Byte offset of this run's `<w:t` opening bracket. Everything in
    /// `[run_start, t_open_start)` is the run's own opening tag plus its
    /// optional `<w:rPr>...</w:rPr>` -- reused verbatim (or, for a
    /// hidden fragment, with a color injected) when rebuilding a split
    /// run, rather than reconstructed from scratch.
    pub t_open_start: usize,
    /// Byte range of this run's whole `<w:rPr>...</w:rPr>` (or
    /// self-closing `<w:rPr/>`), if it has one at all. `None` for a run
    /// with no run properties element -- a hidden fragment then needs a
    /// brand new `<w:rPr>` inserted rather than one to clone and modify.
    pub rpr_range: Option<(usize, usize)>,
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
    /// none).
    pub fn run_containing(&self, flat_start: usize, flat_end: usize) -> Option<&RunSpan> {
        self.runs
            .iter()
            .find(|run| run.flat_start <= flat_start && flat_end <= run.flat_end)
    }

    /// Every run index touched by `[flat_start, flat_end)`, in document
    /// order -- one for the common case, more when the range spans a
    /// run boundary, empty if the range corresponds to no run at all
    /// (out-of-bounds coordinates). See [`crate::edit::apply_edits`] for
    /// how a multi-run result gets spliced.
    ///
    /// A zero-width range (`flat_start == flat_end`, an insertion point
    /// rather than a span) is handled as a special case: it's resolved
    /// inclusively on both ends, exactly like [`Self::run_containing`],
    /// so a point sitting precisely on the boundary between two runs
    /// resolves to the earlier one -- e.g. inserting immediately after
    /// a label's own text lands inside the label's run even when the
    /// blank that follows it is itself split across several further
    /// runs. The strict, exclusive overlap test used for a real span
    /// would otherwise match *nothing* for a zero-width range landing
    /// exactly on a boundary.
    pub(crate) fn runs_touching(&self, flat_start: usize, flat_end: usize) -> Vec<usize> {
        if flat_start == flat_end {
            return self
                .runs
                .iter()
                .position(|run| run.flat_start <= flat_start && flat_end <= run.flat_end)
                .into_iter()
                .collect();
        }

        self.runs
            .iter()
            .enumerate()
            .filter(|(_, run)| run.flat_end > flat_start && run.flat_start < flat_end)
            .map(|(index, _)| index)
            .collect()
    }

    /// Whether `apply_edits` could act on `[flat_start, flat_end)` at
    /// all -- true for anything it can splice (one run, several spliced
    /// together, or a zero-width insert landing on a run boundary),
    /// false only for coordinates that touch no run whatsoever. Lets a
    /// caller validate a batch of candidate edits up front and report
    /// exactly which ones are the problem, rather than letting
    /// `apply_edits` fail the whole batch on the first bad one.
    pub fn is_editable_range(&self, flat_start: usize, flat_end: usize) -> bool {
        !self.runs_touching(flat_start, flat_end).is_empty()
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    let mut current_run_start = 0usize;
    let mut current_rpr_open_start = 0usize;
    let mut current_rpr_range: Option<(usize, usize)> = None;
    let mut pushed_run_this_element = false;

    loop {
        let pos_before = reader.buffer_position() as usize;
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
                    current_run_start = pos_before;
                    current_rpr_range = None;
                    pushed_run_this_element = false;
                }
                b"rPr" => {
                    in_run_props = true;
                    current_rpr_open_start = pos_before;
                }
                b"t" => {
                    let t_open_start = pos_before;
                    let content_start = reader.buffer_position() as usize;
                    let mut decoded = String::new();
                    let mut content_end = content_start;
                    // quick-xml 0.41 (unlike 0.36) splits an entity or
                    // character reference (`&amp;`, `&#39;`) out of
                    // Event::Text into its own Event::GeneralRef -- a
                    // `<w:t>` containing one used to arrive as a single
                    // Text event, now arrives as Text/GeneralRef/Text (or
                    // just GeneralRef, if the reference is the whole
                    // content). Loop and accumulate every content event
                    // up to the closing tag instead of reading exactly
                    // one, so a `<w:t>Smith &amp; Sons</w:t>` still
                    // decodes to one string. An empty `<w:t></w:t>` still
                    // falls out of this loop with `decoded` empty and
                    // `content_end == content_start`, matching the prior
                    // explicit zero-length case.
                    loop {
                        match reader.read_event() {
                            Ok(Event::Text(t)) => {
                                content_end = reader.buffer_position() as usize;
                                let charset_decoded = t.decode().unwrap_or_default();
                                let unescaped = quick_xml::escape::unescape(&charset_decoded)
                                    .unwrap_or_default();
                                decoded.push_str(&unescaped);
                            }
                            Ok(Event::GeneralRef(r)) => {
                                content_end = reader.buffer_position() as usize;
                                if let Ok(Some(ch)) = r.resolve_char_ref() {
                                    decoded.push(ch);
                                } else if let Ok(name) = r.decode() {
                                    if let Some(resolved) =
                                        quick_xml::escape::resolve_predefined_entity(&name)
                                    {
                                        decoded.push_str(resolved);
                                    }
                                }
                            }
                            Ok(Event::CData(t)) => {
                                content_end = reader.buffer_position() as usize;
                                decoded.push_str(&t.decode().unwrap_or_default());
                            }
                            Ok(Event::End(_)) | Ok(Event::Eof) | Err(_) => break,
                            _ => break,
                        }
                    }
                    push_run(
                        current_acc(&mut cell_stack, &mut body),
                        &decoded,
                        content_start,
                        content_end,
                        current_run_underlined,
                        current_run_start,
                        t_open_start,
                        current_rpr_range,
                    );
                    pushed_run_this_element = true;
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
                b"rPr" => {
                    // Self-closing <w:rPr/> -- an empty run-properties element.
                    current_rpr_range = Some((pos_before, reader.buffer_position() as usize));
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
                        current_run_start,
                        pos_before,
                        current_rpr_range,
                    );
                    pushed_run_this_element = true;
                }
                _ => {}
            },

            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"rPr" => {
                    in_run_props = false;
                    current_rpr_range =
                        Some((current_rpr_open_start, reader.buffer_position() as usize));
                }
                b"r" => {
                    if pushed_run_this_element {
                        let run_end = reader.buffer_position() as usize;
                        if let Some(run) = current_acc(&mut cell_stack, &mut body).runs.last_mut() {
                            run.run_end = run_end;
                        }
                    }
                }
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

#[allow(clippy::too_many_arguments)]
fn push_run(
    acc: &mut Accumulator,
    decoded: &str,
    xml_content_start: usize,
    xml_content_end: usize,
    is_underlined: bool,
    run_start: usize,
    t_open_start: usize,
    rpr_range: Option<(usize, usize)>,
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
        run_start,
        // Corrected once the run's own </w:r> is reached -- a run's
        // <w:t> always closes before its own </w:r>, so this is never
        // observed by any caller before then.
        run_end: xml_content_end,
        t_open_start,
        rpr_range,
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
    fn run_start_and_end_bracket_the_whole_run_element() {
        let xml = wrap(r#"<w:p><w:r><w:t>Hello</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;
        let run = flat.runs[0];

        assert_eq!(
            &xml[run.run_start..run.run_end],
            r#"<w:r><w:t>Hello</w:t></w:r>"#
        );
    }

    #[test]
    fn t_open_start_points_at_the_ts_own_opening_tag() {
        let xml = wrap(r#"<w:p><w:r><w:t xml:space="preserve">Hello</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;
        let run = flat.runs[0];

        assert_eq!(
            &xml[run.t_open_start..run.xml_content_start],
            r#"<w:t xml:space="preserve">"#
        );
    }

    #[test]
    fn rpr_range_is_none_when_the_run_has_no_run_properties() {
        let xml = wrap(r#"<w:p><w:r><w:t>Hello</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;

        assert_eq!(flat.runs[0].rpr_range, None);
    }

    #[test]
    fn rpr_range_covers_a_paired_run_properties_element() {
        let xml =
            wrap(r#"<w:p><w:r><w:rPr><w:sz w:val="20"/></w:rPr><w:t>Hello</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;
        let (start, end) = flat.runs[0].rpr_range.unwrap();

        assert_eq!(&xml[start..end], r#"<w:rPr><w:sz w:val="20"/></w:rPr>"#);
    }

    #[test]
    fn rpr_range_covers_a_self_closing_run_properties_element() {
        let xml = wrap(r#"<w:p><w:r><w:rPr/><w:t>Hello</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;
        let (start, end) = flat.runs[0].rpr_range.unwrap();

        assert_eq!(&xml[start..end], r#"<w:rPr/>"#);
    }

    #[test]
    fn run_bounds_are_correct_for_the_second_of_two_runs_in_a_paragraph() {
        let xml =
            wrap(r#"<w:p><w:r><w:t>Tenant: </w:t></w:r><w:r><w:t>John Smith</w:t></w:r></w:p>"#);
        let flat = extract_flat_text(&xml).body;
        let run = flat.runs[1];

        assert_eq!(
            &xml[run.run_start..run.run_end],
            r#"<w:r><w:t>John Smith</w:t></w:r>"#
        );
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
