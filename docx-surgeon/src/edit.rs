use crate::read::{FlatDocument, RegionRef, RunSpan};

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

/// One run's own contribution to an edit -- usually the edit's whole
/// story (the common single-run case), but an edit spanning a run
/// boundary produces one fragment per touched run. Only the *first*
/// touched run's fragment carries the edit's actual replacement text;
/// every other touched run's fragment removes its own portion with
/// nothing put back, since the full replacement already landed exactly
/// once. Local (run-relative) coordinates, not the outer flat-text
/// coordinates an [`Edit`] carries.
struct Fragment {
    local_start: usize,
    local_end: usize,
    text: String,
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
/// [`HiddenBlankEdit`]s to `document_xml` in one combined pass. This is
/// not the same as calling [`apply_edits`] and then
/// [`apply_hidden_blank_edits`] in sequence on each other's output --
/// both kinds of edit compute their replacements against the *same*
/// original `document_xml`'s byte offsets, and those offsets would go
/// stale the moment an intermediate splice changed the document's
/// length. Combining them here, before either's replacements are
/// spliced in, is what makes it safe for one `apply` call to carry a
/// mix of preserve-blank, hide-blank, and plain-value substitutions.
///
/// Critically, this does *not* just concatenate
/// [`compute_plain_replacements`] and [`compute_hidden_replacements`]'s
/// two independently-computed replacement lists -- a real document
/// routinely has several blanks sharing one physical run (Word doesn't
/// give each label-and-blank pair its own run just because a
/// higher-level tool would find that convenient), so a plain edit and a
/// hidden-blank edit can easily both touch the *same* run. Computed
/// independently, the plain side would replace only that run's `<w:t>`
/// content while the hidden side replaced the run's whole element,
/// producing two overlapping byte ranges that corrupt the splice.
/// Routing everything through one shared per-run fragment
/// reconstruction (a plain edit's replacement text simply becomes a
/// "visible" fragment, same shape as a hidden edit's tag fragment)
/// makes that structurally impossible instead of merely unlikely.
pub fn apply_all_edits(
    document_xml: &str,
    doc: &FlatDocument,
    edits: &[Edit],
    hidden_edits: &[HiddenBlankEdit],
) -> Result<String, EditError> {
    check_no_overlaps(edits)?;
    let hidden_spans: Vec<(RegionRef, usize, usize)> = hidden_edits
        .iter()
        .map(|e| (e.region, e.blank_start, e.blank_end))
        .collect();
    check_no_overlapping_spans(&hidden_spans)?;
    check_no_cross_kind_overlap(edits, hidden_edits)?;

    let mut by_run: Vec<(RegionRef, usize, Vec<HiddenEditFragment>)> = Vec::new();
    for edit in edits {
        collect_plain_edit_fragments(doc, edit, &mut by_run)?;
    }
    for edit in hidden_edits {
        collect_hidden_edit_fragments(doc, edit, &mut by_run)?;
    }

    Ok(splice_replacements(
        document_xml,
        finalize_fragment_replacements(document_xml, doc, by_run),
    ))
}

/// A plain edit and a hidden-blank edit in the *same region* whose
/// spans overlap -- checked separately from each kind's own internal
/// overlap check, since those only ever compare edits of their own
/// kind against each other.
fn check_no_cross_kind_overlap(
    edits: &[Edit],
    hidden_edits: &[HiddenBlankEdit],
) -> Result<(), EditError> {
    for edit in edits {
        for hidden in hidden_edits {
            if edit.region == hidden.region
                && hidden.blank_start < edit.flat_end
                && edit.flat_start < hidden.blank_end
            {
                return Err(EditError::OverlappingEdits {
                    region: edit.region,
                    first: (edit.flat_start, edit.flat_end),
                    second: (hidden.blank_start, hidden.blank_end),
                });
            }
        }
    }
    Ok(())
}

/// Computes, but does not yet splice in, the `[xml_content_start,
/// xml_content_end)` replacement for every run touched by `edits`. See
/// [`apply_edits`]'s doc comment for the splicing rules this implements.
fn compute_plain_replacements(
    doc: &FlatDocument,
    edits: &[Edit],
) -> Result<Vec<(usize, usize, String)>, EditError> {
    check_no_overlaps(edits)?;

    // Group fragments by the (region, run) they land in -- more than
    // one edit can legitimately target the same run, and now a single
    // edit spanning multiple runs contributes one fragment per run too.
    // A run index is only unique *within* its own region, never across
    // regions.
    let mut by_run: Vec<(RegionRef, usize, Vec<Fragment>)> = Vec::new();
    for edit in edits {
        let flat = doc.region(edit.region);
        let touched = flat.runs_touching(edit.flat_start, edit.flat_end);
        if touched.is_empty() {
            return Err(EditError::NoMatchingRun {
                region: edit.region,
                flat_start: edit.flat_start,
                flat_end: edit.flat_end,
            });
        }

        for (position, &run_index) in touched.iter().enumerate() {
            let run = flat.runs[run_index];
            let local_start = edit.flat_start.max(run.flat_start) - run.flat_start;
            let local_end = edit.flat_end.min(run.flat_end) - run.flat_start;
            let text = if position == 0 {
                edit.replacement.clone()
            } else {
                String::new()
            };
            let fragment = Fragment {
                local_start,
                local_end,
                text,
            };

            match by_run
                .iter_mut()
                .find(|(region, idx, _)| *region == edit.region && *idx == run_index)
            {
                Some((_, _, group)) => group.push(fragment),
                None => by_run.push((edit.region, run_index, vec![fragment])),
            }
        }
    }

    // For each affected run, splice its fragments into the run's own
    // decoded text (local coordinates), then re-encode the whole thing.
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    for (region, run_index, mut group) in by_run {
        let flat = doc.region(region);
        let run = flat.runs[run_index];
        group.sort_by_key(|f| f.local_start);

        let original = &flat.text[run.flat_start..run.flat_end];
        let mut rebuilt = String::new();
        let mut cursor = 0usize;
        for fragment in group {
            rebuilt.push_str(&original[cursor..fragment.local_start]);
            rebuilt.push_str(&fragment.text);
            cursor = fragment.local_end;
        }
        rebuilt.push_str(&original[cursor..]);

        replacements.push((
            run.xml_content_start,
            run.xml_content_end,
            xml_escape_text(&rebuilt),
        ));
    }

    Ok(replacements)
}

/// Copies `document_xml` through unchanged except for `replacements`
/// (each a `[start, end)` byte range and the new content to put there),
/// applied in order. Shared by every edit kind this crate supports --
/// each kind only differs in *how* it computes its replacements, never
/// in how they get spliced in.
fn splice_replacements(
    document_xml: &str,
    mut replacements: Vec<(usize, usize, String)>,
) -> String {
    replacements.sort_by_key(|(start, _, _)| *start);

    let mut result = String::with_capacity(document_xml.len());
    let mut cursor = 0usize;
    for (start, end, new_content) in replacements {
        result.push_str(&document_xml[cursor..start]);
        result.push_str(&new_content);
        cursor = end;
    }
    result.push_str(&document_xml[cursor..]);

    result
}

/// The color a hidden fragment's `<w:rPr>` gets set to -- matches a
/// white page background, the only background color assumed here (see
/// [`apply_hidden_blank_edits`]'s doc comment).
const HIDDEN_TEXT_COLOR: &str = "FFFFFF";

/// One "hide the blank instead of removing it" edit: every original
/// character in `[blank_start, blank_end)` survives untouched *except*
/// the inner `[tag_start, tag_end)` sub-range, which is replaced by
/// `replacement` -- identical span math to the ordinary preserve-blank
/// [`Edit`] (see [`crate`]'s crate-level docs for why blanks are
/// centered rather than prepended). The difference is what happens to
/// the surviving characters on either side: instead of staying visible
/// (the preserve-blank style), they're recolored to
/// [`HIDDEN_TEXT_COLOR`] so they read as blank on a white page, while
/// remaining the exact original characters -- and therefore the exact
/// original width -- underneath.
///
/// This exists because a `.docx` blank drawn with literal underscore
/// characters (rather than a table cell, a tab stop, or a real form
/// field) has nothing holding the surrounding layout in place except
/// those characters' own rendered width. Deleting them and padding
/// with an equal *count* of spaces does not preserve that width: an
/// underscore's glyph is wider than a space's in every font this was
/// checked against (confirmed against the real Sumas Mini Storage
/// template, Times New Roman 10pt -- removing the underscores and
/// padding with spaces left the surrounding fields visibly bunched
/// together despite the character count matching exactly). Keeping the
/// original characters and only ever changing their *color* keeps the
/// width provably identical, in any font, without needing to know
/// anything about font metrics at all.
#[derive(Debug, Clone)]
pub struct HiddenBlankEdit {
    pub region: RegionRef,
    pub blank_start: usize,
    pub blank_end: usize,
    pub tag_start: usize,
    pub tag_end: usize,
    pub replacement: String,
}

/// Whether a fragment inside a rebuilt run keeps its original color
/// (`Visible`) or gets recolored to [`HIDDEN_TEXT_COLOR`] (`Hidden`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FragmentVisibility {
    Visible,
    Hidden,
}

struct HiddenEditFragment {
    local_start: usize,
    local_end: usize,
    text: String,
    visibility: FragmentVisibility,
}

/// Applies every [`HiddenBlankEdit`] to `document_xml`, returning the
/// modified XML. Assumes a white page background -- every template
/// seen in this corpus so far uses one, and there is no OOXML-level
/// signal cheap enough to check otherwise (a document's actual
/// rendered background can come from theme, section, or printer
/// settings, not one simple property to read).
///
/// Unlike [`apply_edits`], a run touched by a `HiddenBlankEdit` is not
/// necessarily left as one run -- when only *part* of a run falls
/// inside a hidden or replaced sub-range (the label and blank sharing
/// one run, e.g. the real `"DATE: _"` case, where the label must stay
/// visible and only the trailing `_` gets hidden), that run is rebuilt
/// as multiple sibling `<w:r>` elements, one per contiguous
/// visible/hidden/replaced stretch, each cloning the original run's
/// `<w:rPr>` (adding a `<w:color>` override only for a hidden
/// fragment). A run entirely inside one zone is never split -- it's
/// either passed through byte-for-byte unchanged (fully visible, no
/// replacement landing in it) or has its color changed in place (fully
/// hidden), same "touch nothing you don't have to" spirit as
/// [`apply_edits`].
///
/// Every byte of `document_xml` outside a touched run's own
/// `[run_start, run_end)` is copied through unchanged.
pub fn apply_hidden_blank_edits(
    document_xml: &str,
    doc: &FlatDocument,
    edits: &[HiddenBlankEdit],
) -> Result<String, EditError> {
    let replacements = compute_hidden_replacements(document_xml, doc, edits)?;
    Ok(splice_replacements(document_xml, replacements))
}

/// Computes, but does not yet splice in, the `[run_start, run_end)`
/// replacement for every run touched by `edits`. See
/// [`apply_hidden_blank_edits`]'s doc comment for the splitting/
/// recoloring rules this implements.
fn compute_hidden_replacements(
    document_xml: &str,
    doc: &FlatDocument,
    edits: &[HiddenBlankEdit],
) -> Result<Vec<(usize, usize, String)>, EditError> {
    let spans: Vec<(RegionRef, usize, usize)> = edits
        .iter()
        .map(|e| (e.region, e.blank_start, e.blank_end))
        .collect();
    check_no_overlapping_spans(&spans)?;

    let mut by_run: Vec<(RegionRef, usize, Vec<HiddenEditFragment>)> = Vec::new();
    for edit in edits {
        collect_hidden_edit_fragments(doc, edit, &mut by_run)?;
    }

    Ok(finalize_fragment_replacements(document_xml, doc, by_run))
}

/// Adds `edit`'s fragments (as plain, always-[`FragmentVisibility::Visible`]
/// pieces) to `by_run`, grouped by the `(region, run)` they land in --
/// the plain-edit counterpart to [`collect_hidden_edit_fragments`], so
/// both kinds can be reconstructed by the same
/// [`finalize_fragment_replacements`] pass.
fn collect_plain_edit_fragments(
    doc: &FlatDocument,
    edit: &Edit,
    by_run: &mut Vec<(RegionRef, usize, Vec<HiddenEditFragment>)>,
) -> Result<(), EditError> {
    let flat = doc.region(edit.region);
    let touched = flat.runs_touching(edit.flat_start, edit.flat_end);
    if touched.is_empty() {
        return Err(EditError::NoMatchingRun {
            region: edit.region,
            flat_start: edit.flat_start,
            flat_end: edit.flat_end,
        });
    }

    for (position, &run_index) in touched.iter().enumerate() {
        let run = flat.runs[run_index];
        let local_start = edit.flat_start.max(run.flat_start) - run.flat_start;
        let local_end = edit.flat_end.min(run.flat_end) - run.flat_start;
        let text = if position == 0 {
            edit.replacement.clone()
        } else {
            String::new()
        };
        let fragment = HiddenEditFragment {
            local_start,
            local_end,
            text,
            visibility: FragmentVisibility::Visible,
        };

        match by_run
            .iter_mut()
            .find(|(region, idx, _)| *region == edit.region && *idx == run_index)
        {
            Some((_, _, group)) => group.push(fragment),
            None => by_run.push((edit.region, run_index, vec![fragment])),
        }
    }
    Ok(())
}

/// Adds `edit`'s fragments (hidden flanks plus its visible tag) to
/// `by_run`, grouped by the `(region, run)` they land in.
fn collect_hidden_edit_fragments(
    doc: &FlatDocument,
    edit: &HiddenBlankEdit,
    by_run: &mut Vec<(RegionRef, usize, Vec<HiddenEditFragment>)>,
) -> Result<(), EditError> {
    let flat = doc.region(edit.region);
    let touched = flat.runs_touching(edit.blank_start, edit.blank_end);
    if touched.is_empty() {
        return Err(EditError::NoMatchingRun {
            region: edit.region,
            flat_start: edit.blank_start,
            flat_end: edit.blank_end,
        });
    }

    // Which touched run actually carries the replacement text -- the
    // first (only, in the common case) one whose range intersects
    // [tag_start, tag_end). Every other touched run that also
    // intersects it (the tag itself spanning a run boundary) gets that
    // portion emptied instead, same "replacement lands exactly once"
    // rule as `apply_edits`.
    let tag_run_position = touched.iter().position(|&run_index| {
        let run = flat.runs[run_index];
        run.flat_end > edit.tag_start && run.flat_start < edit.tag_end
    });

    for (position, &run_index) in touched.iter().enumerate() {
        let run = flat.runs[run_index];
        let mut fragments = Vec::new();

        push_zone_fragment(
            &mut fragments,
            &flat.text,
            run,
            edit.blank_start,
            edit.tag_start,
            FragmentVisibility::Hidden,
            None,
        );
        push_zone_fragment(
            &mut fragments,
            &flat.text,
            run,
            edit.tag_start,
            edit.tag_end,
            FragmentVisibility::Visible,
            Some(if Some(position) == tag_run_position {
                edit.replacement.as_str()
            } else {
                ""
            }),
        );
        push_zone_fragment(
            &mut fragments,
            &flat.text,
            run,
            edit.tag_end,
            edit.blank_end,
            FragmentVisibility::Hidden,
            None,
        );

        match by_run
            .iter_mut()
            .find(|(region, idx, _)| *region == edit.region && *idx == run_index)
        {
            Some((_, _, group)) => group.extend(fragments),
            None => by_run.push((edit.region, run_index, fragments)),
        }
    }
    Ok(())
}

/// Rebuilds the full `<w:r>...</w:r>` XML for every run named in
/// `by_run`, from its fragments (each already carrying its own
/// visibility). Shared by [`compute_hidden_replacements`] and
/// [`apply_all_edits`] -- the only difference between "a run touched by
/// hidden-blank edits only" and "a run touched by a mix of plain and
/// hidden-blank edits" is which fragments it received, not how they get
/// turned into XML.
fn finalize_fragment_replacements(
    document_xml: &str,
    doc: &FlatDocument,
    by_run: Vec<(RegionRef, usize, Vec<HiddenEditFragment>)>,
) -> Vec<(usize, usize, String)> {
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    for (region, run_index, mut fragments) in by_run {
        let flat = doc.region(region);
        let run = flat.runs[run_index];
        fragments.sort_by_key(|f| f.local_start);

        let original = &flat.text[run.flat_start..run.flat_end];
        let mut pieces: Vec<(String, FragmentVisibility)> = Vec::new();
        let mut cursor = 0usize;
        for fragment in &fragments {
            if fragment.local_start > cursor {
                pieces.push((
                    original[cursor..fragment.local_start].to_string(),
                    FragmentVisibility::Visible,
                ));
            }
            pieces.push((fragment.text.clone(), fragment.visibility));
            cursor = fragment.local_end;
        }
        if cursor < original.len() {
            pieces.push((original[cursor..].to_string(), FragmentVisibility::Visible));
        }

        // Merge adjacent same-visibility pieces (keeps a run that's
        // fully one visibility, or has several contiguous fully-hidden
        // characters, from being split more than the edit actually
        // requires) and drop empty pieces entirely -- an emptied
        // "other touched run" fragment, or a zero-width boundary, has
        // nothing left to render either way.
        let mut merged: Vec<(String, FragmentVisibility)> = Vec::new();
        for (text, visibility) in pieces {
            if text.is_empty() {
                continue;
            }
            match merged.last_mut() {
                Some((last_text, last_visibility)) if *last_visibility == visibility => {
                    last_text.push_str(&text);
                }
                _ => merged.push((text, visibility)),
            }
        }

        let run_xml = if merged.is_empty() {
            build_run_xml(document_xml, &run, "", FragmentVisibility::Visible)
        } else {
            merged
                .into_iter()
                .map(|(text, visibility)| build_run_xml(document_xml, &run, &text, visibility))
                .collect::<Vec<_>>()
                .join("")
        };

        replacements.push((run.run_start, run.run_end, run_xml));
    }

    replacements
}

/// Pushes a fragment for the portion of `[zone_start, zone_end)` that
/// falls inside `run`, if any. `replacement_text`, when given,
/// overrides the fragment's text with that literal string (used for
/// the tag zone, whose content isn't a slice of the original text at
/// all) -- `None` means "use the original characters unchanged" (the
/// two hidden zones, which keep their real underscores).
#[allow(clippy::too_many_arguments)]
fn push_zone_fragment(
    fragments: &mut Vec<HiddenEditFragment>,
    flat_text: &str,
    run: RunSpan,
    zone_start: usize,
    zone_end: usize,
    visibility: FragmentVisibility,
    replacement_text: Option<&str>,
) {
    let start = zone_start.max(run.flat_start);
    let end = zone_end.min(run.flat_end).max(start);
    if end <= start {
        return;
    }

    let text = match replacement_text {
        Some(replacement) => replacement.to_string(),
        None => flat_text[start..end].to_string(),
    };

    fragments.push(HiddenEditFragment {
        local_start: start - run.flat_start,
        local_end: end - run.flat_start,
        text,
        visibility,
    });
}

/// Rebuilds one run's whole `<w:r>...</w:r>` XML with `text` as its
/// content, reusing every other byte of the run's original structure
/// (its opening tag's own attributes, its `<w:t>` tag's own attributes,
/// its closing tags) verbatim. For a hidden fragment, the run's
/// `<w:rPr>` is cloned and given a color override; a run with no
/// `<w:rPr>` at all gets a brand new one inserted.
fn build_run_xml(
    document_xml: &str,
    run: &RunSpan,
    text: &str,
    visibility: FragmentVisibility,
) -> String {
    let prefix = match visibility {
        FragmentVisibility::Visible => document_xml[run.run_start..run.t_open_start].to_string(),
        FragmentVisibility::Hidden => hidden_run_prefix_xml(document_xml, run),
    };
    let t_open = &document_xml[run.t_open_start..run.xml_content_start];
    let t_and_r_close = &document_xml[run.xml_content_end..run.run_end];

    format!("{prefix}{t_open}{}{t_and_r_close}", xml_escape_text(text))
}

/// The run's own opening tag plus a `<w:rPr>` guaranteed to carry a
/// [`HIDDEN_TEXT_COLOR`] `<w:color>` -- cloning and augmenting the
/// original `<w:rPr>` if the run has one, otherwise inserting a new,
/// minimal one right after the opening tag.
fn hidden_run_prefix_xml(document_xml: &str, run: &RunSpan) -> String {
    match run.rpr_range {
        Some((rpr_start, rpr_end)) => {
            let before_rpr = &document_xml[run.run_start..rpr_start];
            let rpr_xml = &document_xml[rpr_start..rpr_end];
            let after_rpr = &document_xml[rpr_end..run.t_open_start];
            format!("{before_rpr}{}{after_rpr}", recolor_run_props_xml(rpr_xml))
        }
        None => {
            let opening_tag = &document_xml[run.run_start..run.t_open_start];
            format!("{opening_tag}<w:rPr><w:color w:val=\"{HIDDEN_TEXT_COLOR}\"/></w:rPr>")
        }
    }
}

/// Returns `rpr_xml` (a whole `<w:rPr>...</w:rPr>` or self-closing
/// `<w:rPr/>`) with any existing `<w:color>` child removed and a
/// [`HIDDEN_TEXT_COLOR`] one inserted as the *first* child. First is
/// schema-safe for every `<w:rPr>` shape seen in this project's real
/// corpus so far (`sz`/`szCs`/`u` all sort after `color` in OOXML's
/// declared `CT_RPr` child sequence) -- a hypothetical run that already
/// overrides an earlier-sequenced property (`rFonts`, `b`, `i`, ...) is
/// not specially handled, since none has been seen in practice yet.
fn recolor_run_props_xml(rpr_xml: &str) -> String {
    let without_existing_color = remove_color_element(rpr_xml);
    let color_element = format!("<w:color w:val=\"{HIDDEN_TEXT_COLOR}\"/>");

    if let Some(stripped) = without_existing_color.strip_suffix("/>") {
        // Self-closing <w:rPr/> (no children) -- becomes a paired tag
        // with the color as its only child.
        format!("{stripped}>{color_element}</w:rPr>")
    } else if let Some(prefix) = without_existing_color.strip_suffix("</w:rPr>") {
        let open_tag_end = prefix.find('>').map(|i| i + 1).unwrap_or(prefix.len());
        format!(
            "{}{color_element}{}</w:rPr>",
            &prefix[..open_tag_end],
            &prefix[open_tag_end..]
        )
    } else {
        // Doesn't match either known <w:rPr> shape -- leave it
        // untouched rather than guess at one that doesn't apply here.
        without_existing_color
    }
}

/// Strips any existing self-closing `<w:color .../>` element out of
/// `rpr_xml` -- Word always writes `color` self-closing (it never has
/// children), so a paired `<w:color>...</w:color>` form is not
/// expected and not handled. Leaving a stale color behind would either
/// produce schema-invalid XML (two `<w:color>` siblings) or have the
/// wrong one win, depending on the consumer.
fn remove_color_element(rpr_xml: &str) -> String {
    let Some(start) = rpr_xml.find("<w:color") else {
        return rpr_xml.to_string();
    };
    let Some(relative_end) = rpr_xml[start..].find("/>") else {
        return rpr_xml.to_string();
    };
    let end = start + relative_end + "/>".len();

    let mut result = String::with_capacity(rpr_xml.len());
    result.push_str(&rpr_xml[..start]);
    result.push_str(&rpr_xml[end..]);
    result
}

/// Overlap is only meaningful *within* one region -- two edits in
/// different regions can share identical numeric coordinates without
/// conflicting at all, since those coordinates are relative to
/// different text entirely.
fn check_no_overlaps(edits: &[Edit]) -> Result<(), EditError> {
    let spans: Vec<(RegionRef, usize, usize)> = edits
        .iter()
        .map(|e| (e.region, e.flat_start, e.flat_end))
        .collect();
    check_no_overlapping_spans(&spans)
}

/// Shared by [`check_no_overlaps`] and [`HiddenBlankEdit`]'s own
/// overlap check -- both ultimately just need "no two spans in the
/// same region may overlap," differing only in which struct fields
/// the span's `(start, end)` come from.
fn check_no_overlapping_spans(spans: &[(RegionRef, usize, usize)]) -> Result<(), EditError> {
    let mut seen_regions: Vec<RegionRef> = Vec::new();
    for &(region, _, _) in spans {
        if !seen_regions.contains(&region) {
            seen_regions.push(region);
        }
    }

    for region in seen_regions {
        let mut same_region: Vec<(usize, usize)> = spans
            .iter()
            .filter(|(r, _, _)| *r == region)
            .map(|(_, start, end)| (*start, *end))
            .collect();
        same_region.sort_by_key(|(start, _)| *start);
        for pair in same_region.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if b.0 < a.1 {
                return Err(EditError::OverlappingEdits {
                    region,
                    first: a,
                    second: b,
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

    fn hidden_body_edit(
        blank_start: usize,
        blank_end: usize,
        tag_start: usize,
        tag_end: usize,
        replacement: &str,
    ) -> HiddenBlankEdit {
        HiddenBlankEdit {
            region: RegionRef::Body,
            blank_start,
            blank_end,
            tag_start,
            tag_end,
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
    fn hidden_blank_splits_a_pure_underscore_run_into_hidden_tag_hidden() {
        // 30 underscores, "{{m.indate}}" is 12 chars -- 18 chars of
        // padding split 9/9, same math as the visible PreserveBlank
        // style, but here the flanks get recolored instead of shown.
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
        let tag = "{{m.indate}}";
        let tag_start = blank_start + 9;
        let tag_end = tag_start + tag.len();

        let edited = apply_hidden_blank_edits(
            &xml,
            &doc,
            &[hidden_body_edit(
                blank_start,
                blank_end,
                tag_start,
                tag_end,
                tag,
            )],
        )
        .unwrap();

        // The underscores are still really there -- just invisible --
        // so the flattened text reads exactly like the visible
        // PreserveBlank style.
        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(
            reflattened.text,
            "Move-In Date: _________{{m.indate}}_________"
        );

        // One run became four: the label text (untouched), hidden left
        // flank, visible tag, hidden right flank.
        assert_eq!(reflattened.runs.len(), 4);
        assert_eq!(edited.matches("<w:color w:val=\"FFFFFF\"/>").count(), 2);
        assert!(edited.contains("<w:t>Move-In Date: </w:t>"));
        assert!(edited.contains("<w:t>_________</w:t>"));
        assert!(edited.contains("<w:t>{{m.indate}}</w:t>"));
    }

    #[test]
    fn hidden_blank_only_hides_the_underscore_sharing_a_run_with_its_label() {
        // The real Sumas Mini Storage shape: the label and the blank's
        // first underscore share one run ("DATE: _"), with the rest of
        // the blank continuing in a second run. The label text must
        // stay fully visible even though it's in the very same run as
        // a character that needs hiding.
        let xml = wrap(
            r#"<w:p><w:r><w:t>DATE: _</w:t></w:r><w:r><w:t>____________________</w:t></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);
        assert_eq!(doc.body.text, "DATE: _____________________"); // "DATE: " + 21 underscores

        let blank_start = "DATE: ".len();
        let blank_end = blank_start + 21;
        let tag = "{{d.now}}";
        let tag_start = blank_start + 6;
        let tag_end = tag_start + tag.len();

        let edited = apply_hidden_blank_edits(
            &xml,
            &doc,
            &[hidden_body_edit(
                blank_start,
                blank_end,
                tag_start,
                tag_end,
                tag,
            )],
        )
        .unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "DATE: ______{{d.now}}______");

        // The label survives as its own untouched, non-recolored run.
        assert!(edited.contains("<w:t>DATE: </w:t>"));
        // Two hidden runs (one carrying the single underscore that
        // shared a run with the label, one carrying the rest of the
        // left flank) plus the visible tag run plus the right flank.
        assert_eq!(edited.matches("<w:color w:val=\"FFFFFF\"/>").count(), 3);
    }

    #[test]
    fn hidden_blank_degrades_to_a_plain_replacement_when_the_blank_is_too_short() {
        // Mirrors PreserveBlank's own degrade case: tag_start ==
        // blank_start and tag_end == blank_end means there's no
        // leftover blank on either side to hide at all.
        let xml = wrap(r#"<w:p><w:r><w:t>Move-In Date: ______</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        let blank_start = doc.body.text.find("______").unwrap();
        let blank_end = blank_start + 6;

        let edited = apply_hidden_blank_edits(
            &xml,
            &doc,
            &[hidden_body_edit(
                blank_start,
                blank_end,
                blank_start,
                blank_end,
                "{{m.indate}}",
            )],
        )
        .unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "Move-In Date: {{m.indate}}");
        assert!(!edited.contains("w:color"));
    }

    #[test]
    fn hidden_blank_clones_the_runs_existing_formatting_onto_the_hidden_fragment() {
        let xml = wrap(
            r#"<w:p><w:r><w:rPr><w:sz w:val="20"/></w:rPr><w:t>SIZE__________</w:t></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);
        let blank_start = doc.body.text.find("__________").unwrap();
        let blank_end = blank_start + 10;
        let tag_start = blank_start + 3;
        let tag_end = tag_start + "{{u.dim}}".len();

        let edited = apply_hidden_blank_edits(
            &xml,
            &doc,
            &[hidden_body_edit(
                blank_start,
                blank_end,
                tag_start,
                tag_end,
                "{{u.dim}}",
            )],
        )
        .unwrap();

        // Both the original size formatting AND the new hidden color
        // must be present on the recolored fragment.
        assert!(edited.contains(r#"<w:rPr><w:color w:val="FFFFFF"/><w:sz w:val="20"/></w:rPr>"#));
    }

    #[test]
    fn hidden_blank_overrides_rather_than_duplicates_an_existing_color() {
        let xml = wrap(
            r#"<w:p><w:r><w:rPr><w:color w:val="FF0000"/></w:rPr><w:t>____</w:t></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);

        let edited =
            apply_hidden_blank_edits(&xml, &doc, &[hidden_body_edit(0, 4, 0, 0, "")]).unwrap();

        assert!(edited.contains(r#"<w:color w:val="FFFFFF"/>"#));
        assert!(!edited.contains("FF0000"));
        assert_eq!(edited.matches("<w:color").count(), 1);
    }

    #[test]
    fn hidden_blank_inserts_a_fresh_rpr_for_a_run_with_no_formatting_at_all() {
        let xml = wrap(r#"<w:p><w:r><w:t>____</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let edited =
            apply_hidden_blank_edits(&xml, &doc, &[hidden_body_edit(0, 4, 0, 0, "")]).unwrap();

        assert!(edited
            .contains(r#"<w:r><w:rPr><w:color w:val="FFFFFF"/></w:rPr><w:t>____</w:t></w:r>"#));
    }

    #[test]
    fn apply_all_edits_combines_a_plain_edit_and_a_hidden_blank_edit_in_one_pass() {
        // A plain value substitution (detect_candidates-style) and a
        // hidden-blank substitution (recognize_blanks-style) both
        // landing in the same `apply` call, on different runs -- must
        // both take effect correctly without either's byte offsets
        // going stale from the other's edit.
        let xml = wrap(
            r#"<w:p><w:r><w:t>Tenant: Atherton Storage</w:t></w:r></w:p><w:p><w:r><w:t>SIZE_____________</w:t></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);

        let value_start = doc.body.text.find("Atherton Storage").unwrap();
        let value_end = value_start + "Atherton Storage".len();
        let blank_start = doc.body.text.find("_____________").unwrap();
        let blank_end = blank_start + 13;
        let tag_start = blank_start + 2;
        let tag_end = tag_start + "{{u.dim}}".len();

        let edited = apply_all_edits(
            &xml,
            &doc,
            &[body_edit(value_start, value_end, "{{f.name}}")],
            &[hidden_body_edit(
                blank_start,
                blank_end,
                tag_start,
                tag_end,
                "{{u.dim}}",
            )],
        )
        .unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "Tenant: {{f.name}}\nSIZE__{{u.dim}}__");
        assert_eq!(edited.matches("<w:color w:val=\"FFFFFF\"/>").count(), 2);
    }

    #[test]
    fn apply_all_edits_handles_a_plain_edit_and_a_hidden_blank_edit_sharing_one_run() {
        // Real documents routinely put several labels and blanks in one
        // physical run -- Word has no reason to break a run just
        // because two different substitutions will eventually land in
        // it. A plain edit only ever replaces a run's own <w:t> content
        // range; a hidden-blank edit replaces the run's *whole*
        // element. Computed independently against the same run, those
        // two replacement ranges would overlap and corrupt the splice
        // -- this reproduces the exact shape that panicked before the
        // two kinds were unified into one per-run reconstruction.
        let xml = wrap(r#"<w:p><w:r><w:t>AAA_____BBB_______________CCC</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);
        assert_eq!(doc.body.runs.len(), 1, "test setup must be a single run");

        let edited = apply_all_edits(
            &xml,
            &doc,
            &[body_edit(3, 8, "{{a}}")],
            &[hidden_body_edit(11, 26, 16, 21, "{{b}}")],
        )
        .unwrap();

        let reflattened = extract_flat_text(&edited).body;
        assert_eq!(reflattened.text, "AAA{{a}}BBB_____{{b}}_____CCC");
        assert_eq!(edited.matches("<w:color w:val=\"FFFFFF\"/>").count(), 2);
    }

    #[test]
    fn hidden_blank_leaves_an_unrelated_sibling_run_byte_identical() {
        let xml = wrap(
            r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Bold label: </w:t></w:r><w:r><w:t>____</w:t></w:r></w:p>"#,
        );
        let doc = extract_flat_text(&xml);
        let blank_run = doc.body.runs[1];

        let edited = apply_hidden_blank_edits(
            &xml,
            &doc,
            &[hidden_body_edit(
                blank_run.flat_start,
                blank_run.flat_end,
                blank_run.flat_start + 1,
                blank_run.flat_start + 2,
                "{{x}}",
            )],
        )
        .unwrap();

        assert!(edited.contains(r#"<w:rPr><w:b/></w:rPr><w:t>Bold label: </w:t>"#));
    }

    #[test]
    fn hidden_blank_refuses_an_edit_touching_no_run_at_all() {
        let xml = wrap(r#"<w:p><w:r><w:t>John</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let result =
            apply_hidden_blank_edits(&xml, &doc, &[hidden_body_edit(100, 110, 100, 110, "x")]);

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
    fn hidden_blank_refuses_overlapping_edits_in_the_same_region() {
        let xml = wrap(r#"<w:p><w:r><w:t>__________</w:t></w:r></w:p>"#);
        let doc = extract_flat_text(&xml);

        let result = apply_hidden_blank_edits(
            &xml,
            &doc,
            &[
                hidden_body_edit(0, 5, 0, 0, "A"),
                hidden_body_edit(3, 10, 3, 3, "B"),
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
}
