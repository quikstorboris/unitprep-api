use quick_xml::events::Event;
use quick_xml::Reader;

use crate::textbox::{room_to_right, GroupBlank, PageBounds, TextBox, TextBoxTracker};

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
    /// `<w:r>`, e.g. [`crate::edit::apply_underline_edits`]/[`crate::edit::apply_all_edits`]) rather
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
    /// Flat-text offset of every `\t` that came from a real `<w:tab/>`
    /// inside an *underlined* run -- how a form draws a fill-in line
    /// without any literal `_` characters (see
    /// `unitprep_template_tagger::recognize_tab_blanks`). Sorted.
    pub underlined_tabs: Vec<usize>,
    /// Where a tag could be inserted into this region if it is a table
    /// cell holding no text at all (an empty first paragraph, no runs)
    /// -- the shape of an "initials" box. `None` otherwise.
    pub empty_paragraph: Option<EmptyParagraph>,
}

/// Where, in the raw `document.xml`, a brand-new run can be written
/// into an otherwise empty table-cell paragraph.
///
/// Either the paragraph is `<w:p ...>...</w:p>` with nothing but
/// (optionally) `<w:pPr>` inside -- `replace_end == insert_at` and the
/// new `<w:r>` is inserted at `insert_at`, just before `</w:p>` -- or
/// it is self-closing, `<w:p .../>`, in which case `[insert_at,
/// replace_end)` is the whole tag and `self_closing_open_tag` holds its
/// text minus the trailing `/>`, so it can be re-emitted as an open tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmptyParagraph {
    pub insert_at: usize,
    pub replace_end: usize,
    pub self_closing_open_tag: Option<String>,
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
            || self.is_empty_paragraph_insertion(flat_start, flat_end)
    }

    /// Whether `[flat_start, flat_end)` is the one insertion point an
    /// empty table cell offers -- see [`EmptyParagraph`].
    pub fn is_empty_paragraph_insertion(&self, flat_start: usize, flat_end: usize) -> bool {
        flat_start == 0 && flat_end == 0 && self.runs.is_empty() && self.empty_paragraph.is_some()
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
    /// Every floating text box in the body, in document order (see
    /// [`crate::textbox`]). The `mc:Fallback` copy of each box is not
    /// read at all -- it duplicates the `mc:Choice` one.
    pub textboxes: Vec<TextBox>,
    pub page: PageBounds,
}

impl FlatDocument {
    /// The text box whose (non-empty) text ends exactly at body flat
    /// offset `flat_end`, if any.
    pub fn textbox_ending_at(&self, flat_end: usize) -> Option<usize> {
        self.textboxes
            .iter()
            .position(|b| b.text_end == flat_end && b.text_start < b.text_end)
    }

    /// How wide (EMU) text box `index` can grow -- `None` if it cannot
    /// be resized or has nothing worth growing into. See
    /// [`crate::textbox::room_to_right`].
    pub fn textbox_room_emu(&self, index: usize) -> Option<i64> {
        room_to_right(&self.textboxes, index, self.page)
    }

    /// Whether a value can be written next to text box `index`: it has
    /// room to grow, or its group shape has a blank square after it.
    pub fn textbox_can_hold_value(&self, index: usize) -> bool {
        self.textbox_room_emu(index).is_some() || self.textboxes[index].group_blank.is_some()
    }

    /// The group blank of the text box whose text ends at `flat_end`.
    pub fn group_blank_ending_at(&self, flat_end: usize) -> Option<&GroupBlank> {
        let index = self.textbox_ending_at(flat_end)?;
        self.textboxes[index].group_blank.as_ref()
    }

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
    underlined_tabs: Vec<usize>,
    empty_paragraph: Option<EmptyParagraph>,
    /// Paragraph nesting depth at which this region's first paragraph
    /// opened, while that paragraph is still open.
    first_paragraph_depth: Option<usize>,
}

impl Accumulator {
    fn new() -> Self {
        Accumulator {
            text: String::new(),
            runs: Vec::new(),
            wrote_any_paragraph: false,
            underlined_tabs: Vec::new(),
            empty_paragraph: None,
            first_paragraph_depth: None,
        }
    }

    fn finish(self) -> FlatText {
        // An insertion point is only valid while the region really has
        // nothing addressable in it.
        let empty_paragraph = if self.runs.is_empty() {
            self.empty_paragraph
        } else {
            None
        };
        FlatText {
            text: self.text,
            runs: self.runs,
            underlined_tabs: self.underlined_tabs,
            empty_paragraph,
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
    // `<w:tab w:val=".." w:pos=".."/>` inside `<w:tabs>` *defines* a tab
    // stop; it is not a tab character in the text.
    let mut in_tab_stops = false;
    let mut paragraph_depth = 0usize;
    let mut outer_paragraph_start = 0usize;
    let mut fallback_depth = 0usize;
    let mut textboxes = TextBoxTracker::default();

    loop {
        let pos_before = reader.buffer_position() as usize;
        let event = reader.read_event();

        // `mc:AlternateContent` carries every floating shape twice: the
        // modern `mc:Choice` and a legacy `mc:Fallback` copy. Only the
        // first is read, so nothing is matched (or edited) twice.
        match &event {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"Fallback" => {
                fallback_depth += 1;
                continue;
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"Fallback" => {
                fallback_depth = fallback_depth.saturating_sub(1);
                continue;
            }
            Ok(Event::Eof) | Err(_) => {}
            _ if fallback_depth > 0 => continue,
            _ => {}
        }

        match &event {
            Ok(Event::Start(e)) => textboxes.on_start(
                e.local_name().as_ref(),
                e,
                pos_before,
                outer_paragraph_start,
            ),
            Ok(Event::Empty(e)) => {
                let tag = &document_xml[pos_before..reader.buffer_position() as usize];
                textboxes.on_empty(e.local_name().as_ref(), tag, pos_before);
            }
            Ok(Event::End(e)) => textboxes.on_end(e.local_name().as_ref()),
            Ok(Event::Text(t)) => textboxes.on_text(&t.decode().unwrap_or_default()),
            _ => {}
        }

        match event {
            Ok(Event::Eof) => break,
            Err(_) => break,

            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                b"tc" => {
                    cell_stack.push(Accumulator::new());
                }
                b"p" => {
                    paragraph_depth += 1;
                    if paragraph_depth == 1 {
                        outer_paragraph_start = pos_before;
                    }
                    let acc = current_acc(&mut cell_stack, &mut body);
                    if acc.wrote_any_paragraph {
                        acc.text.push('\n');
                    } else {
                        acc.first_paragraph_depth = Some(paragraph_depth);
                    }
                    acc.wrote_any_paragraph = true;
                    let flat_len = acc.text.len();
                    textboxes.take_pending_text_start(flat_len);
                }
                b"tabs" => in_tab_stops = true,
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
                b"tab" if in_tab_stops => {}
                b"tab" => {
                    let acc = current_acc(&mut cell_stack, &mut body);
                    if current_run_underlined {
                        acc.underlined_tabs.push(acc.text.len());
                    }
                    acc.text.push('\t');
                }
                b"p" => {
                    // A self-closing paragraph: the first (and so far
                    // only) one in a table cell is an insertion point.
                    let in_cell = !cell_stack.is_empty();
                    let acc = current_acc(&mut cell_stack, &mut body);
                    if in_cell && !acc.wrote_any_paragraph && acc.empty_paragraph.is_none() {
                        let tag_end = reader.buffer_position() as usize;
                        let tag = &document_xml[pos_before..tag_end];
                        if let Some(open) = tag.strip_suffix("/>") {
                            acc.empty_paragraph = Some(EmptyParagraph {
                                insert_at: pos_before,
                                replace_end: tag_end,
                                self_closing_open_tag: Some(open.trim_end().to_string()),
                            });
                        }
                    }
                }
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
                b"tabs" => in_tab_stops = false,
                b"txbxContent" => {
                    textboxes.finish_content(cell_stack.is_empty().then_some(body.text.len()));
                    // The box's text must not run into whatever the
                    // anchoring paragraph says next ("ANOTHER PERSON" +
                    // "IN ADDITION..." must not read as "PERSONIN").
                    current_acc(&mut cell_stack, &mut body).text.push('\n');
                }
                b"p" => {
                    let in_cell = !cell_stack.is_empty();
                    let acc = current_acc(&mut cell_stack, &mut body);
                    if in_cell
                        && acc.first_paragraph_depth == Some(paragraph_depth)
                        && acc.runs.is_empty()
                        && acc.empty_paragraph.is_none()
                    {
                        acc.empty_paragraph = Some(EmptyParagraph {
                            insert_at: pos_before,
                            replace_end: pos_before,
                            self_closing_open_tag: None,
                        });
                    }
                    if acc.first_paragraph_depth == Some(paragraph_depth) {
                        acc.first_paragraph_depth = None;
                    }
                    paragraph_depth = paragraph_depth.saturating_sub(1);
                }
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

    let body = body.finish();
    let mut boxes = textboxes.boxes;
    for text_box in &mut boxes {
        // A group blank's tag is written in the label's own font.
        if let Some(blank) = &mut text_box.group_blank {
            blank.run_props = body
                .runs
                .iter()
                .find(|run| {
                    run.flat_start >= text_box.text_start
                        && run.flat_end <= text_box.text_end
                        && run.flat_end > run.flat_start
                })
                .and_then(|run| run.rpr_range);
        }
    }

    FlatDocument {
        body,
        table_cells,
        textboxes: boxes,
        page: textboxes.page,
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

    #[test]
    fn an_underlined_tab_is_recorded_but_a_plain_one_is_not() {
        let xml = wrap(
            r#"<w:p><w:r><w:t>Date:</w:t></w:r><w:r><w:rPr><w:u w:val="single"/></w:rPr><w:tab/></w:r><w:r><w:t>Of:</w:t></w:r><w:r><w:tab/></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);

        assert_eq!(doc.body.text, "Date:\tOf:\t");
        assert_eq!(doc.body.underlined_tabs, vec![5]);
    }

    #[test]
    fn a_tab_stop_definition_is_not_a_tab_character() {
        let xml = wrap(
            r#"<w:p><w:pPr><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs></w:pPr><w:r><w:t>A</w:t></w:r><w:r><w:tab/></w:r><w:r><w:t>B</w:t></w:r></w:p>"#,
        );

        assert_eq!(extract_flat_text(&xml).body.text, "A\tB");
    }

    #[test]
    fn an_empty_cell_paragraph_is_an_insertion_point() {
        let xml = wrap(r#"<w:tbl><w:tr><w:tc><w:p><w:pPr/></w:p></w:tc></w:tr></w:tbl>"#);
        let doc = extract_flat_text(&xml);

        let paragraph = doc.table_cells[0].empty_paragraph.as_ref().unwrap();
        assert_eq!(
            &xml[paragraph.insert_at..],
            "</w:p></w:tc></w:tr></w:tbl></w:body></w:document>"
        );
        assert_eq!(paragraph.insert_at, paragraph.replace_end);
        assert!(doc.table_cells[0].is_editable_range(0, 0));
    }

    #[test]
    fn a_self_closing_cell_paragraph_is_an_insertion_point() {
        let xml = wrap(r#"<w:tbl><w:tr><w:tc><w:p/></w:tc></w:tr></w:tbl>"#);
        let doc = extract_flat_text(&xml);

        let paragraph = doc.table_cells[0].empty_paragraph.as_ref().unwrap();
        assert_eq!(&xml[paragraph.insert_at..paragraph.replace_end], "<w:p/>");
        assert_eq!(paragraph.self_closing_open_tag.as_deref(), Some("<w:p"));
    }

    #[test]
    fn a_cell_with_text_or_body_text_has_no_insertion_point() {
        let xml = wrap(&format!(
            r#"<w:p></w:p><w:tbl><w:tr>{}</w:tr></w:tbl>"#,
            cell("Value")
        ));
        let doc = extract_flat_text(&xml);

        assert!(doc.body.empty_paragraph.is_none());
        assert!(doc.table_cells[0].empty_paragraph.is_none());
        assert!(!doc.table_cells[0].is_empty_paragraph_insertion(0, 0));
    }
}
