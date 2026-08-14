use crate::read::{FlatDocument, RegionRef};

use super::overlap::{check_no_overlapping_spans, check_no_overlaps};
use super::run_xml::{build_run_xml, xml_escape_text};
use super::{Edit, EditError, UnderlineEdit};

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

/// Computes, but does not yet splice in, the `[xml_content_start,
/// xml_content_end)` replacement for every run touched by `edits`. See
/// [`super::apply_edits`]'s doc comment for the splicing rules this
/// implements.
pub(super) fn compute_plain_replacements(
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
pub(super) fn splice_replacements(
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

/// Whether a fragment inside a rebuilt run keeps its original
/// formatting (`Original`) or gets a `<w:u w:val="single"/>` override
/// (`Underlined`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RunStyle {
    Original,
    Underlined,
}

pub(super) struct StyledFragment {
    local_start: usize,
    local_end: usize,
    text: String,
    style: RunStyle,
}

/// Computes, but does not yet splice in, the `[run_start, run_end)`
/// replacement for every run touched by `edits`. See
/// [`super::apply_underline_edits`]'s doc comment for the splitting
/// rules this implements.
pub(super) fn compute_underline_replacements(
    document_xml: &str,
    doc: &FlatDocument,
    edits: &[UnderlineEdit],
) -> Result<Vec<(usize, usize, String)>, EditError> {
    let spans: Vec<(RegionRef, usize, usize)> = edits
        .iter()
        .map(|e| (e.region, e.flat_start, e.flat_end))
        .collect();
    check_no_overlapping_spans(&spans)?;

    let mut by_run: Vec<(RegionRef, usize, Vec<StyledFragment>)> = Vec::new();
    for edit in edits {
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

    Ok(finalize_fragment_replacements(document_xml, doc, by_run))
}

/// Adds one edit's fragments to `by_run`, grouped by the `(region,
/// run)` they land in -- shared by a plain [`Edit`] (`style:
/// RunStyle::Original`) and an [`UnderlineEdit`] (`style:
/// RunStyle::Underlined`); the only difference between the two is which
/// style their replacement fragment carries; everything about *finding*
/// the touched runs and the "replacement lands exactly once" rule is
/// identical.
#[allow(clippy::too_many_arguments)]
pub(super) fn collect_fragments(
    doc: &FlatDocument,
    region: RegionRef,
    flat_start: usize,
    flat_end: usize,
    replacement: &str,
    replacement_style: RunStyle,
    by_run: &mut Vec<(RegionRef, usize, Vec<StyledFragment>)>,
) -> Result<(), EditError> {
    let flat = doc.region(region);
    let touched = flat.runs_touching(flat_start, flat_end);
    if touched.is_empty() {
        return Err(EditError::NoMatchingRun {
            region,
            flat_start,
            flat_end,
        });
    }

    for (position, &run_index) in touched.iter().enumerate() {
        let run = flat.runs[run_index];
        let local_start = flat_start.max(run.flat_start) - run.flat_start;
        let local_end = flat_end.min(run.flat_end) - run.flat_start;
        let text = if position == 0 {
            replacement.to_string()
        } else {
            String::new()
        };
        let fragment = StyledFragment {
            local_start,
            local_end,
            text,
            style: replacement_style,
        };

        match by_run
            .iter_mut()
            .find(|(r, idx, _)| *r == region && *idx == run_index)
        {
            Some((_, _, group)) => group.push(fragment),
            None => by_run.push((region, run_index, vec![fragment])),
        }
    }
    Ok(())
}

/// Rebuilds the full `<w:r>...</w:r>` XML for every run named in
/// `by_run`, from its fragments (each already carrying its own style).
/// Shared by [`compute_underline_replacements`] and
/// [`super::apply_all_edits`] -- the only difference between "a run
/// touched by underline edits only" and "a run touched by a mix of
/// plain and underline edits" is which fragments it received, not how
/// they get turned into XML.
pub(super) fn finalize_fragment_replacements(
    document_xml: &str,
    doc: &FlatDocument,
    by_run: Vec<(RegionRef, usize, Vec<StyledFragment>)>,
) -> Vec<(usize, usize, String)> {
    let mut replacements: Vec<(usize, usize, String)> = Vec::new();
    for (region, run_index, mut fragments) in by_run {
        let flat = doc.region(region);
        let run = flat.runs[run_index];
        fragments.sort_by_key(|f| f.local_start);

        let original = &flat.text[run.flat_start..run.flat_end];
        let mut pieces: Vec<(String, RunStyle)> = Vec::new();
        let mut cursor = 0usize;
        for fragment in &fragments {
            if fragment.local_start > cursor {
                pieces.push((
                    original[cursor..fragment.local_start].to_string(),
                    RunStyle::Original,
                ));
            }
            pieces.push((fragment.text.clone(), fragment.style));
            cursor = fragment.local_end;
        }
        if cursor < original.len() {
            pieces.push((original[cursor..].to_string(), RunStyle::Original));
        }

        // Merge adjacent same-style pieces (keeps a run that's fully
        // one style from being split more than the edit actually
        // requires) and drop empty pieces entirely -- an emptied "other
        // touched run" fragment, or a zero-width boundary, has nothing
        // left to render either way.
        let mut merged: Vec<(String, RunStyle)> = Vec::new();
        for (text, style) in pieces {
            if text.is_empty() {
                continue;
            }
            match merged.last_mut() {
                Some((last_text, last_style)) if *last_style == style => {
                    last_text.push_str(&text);
                }
                _ => merged.push((text, style)),
            }
        }

        let run_xml = if merged.is_empty() {
            build_run_xml(document_xml, &run, "", RunStyle::Original)
        } else {
            merged
                .into_iter()
                .map(|(text, style)| build_run_xml(document_xml, &run, &text, style))
                .collect::<Vec<_>>()
                .join("")
        };

        replacements.push((run.run_start, run.run_end, run_xml));
    }

    replacements
}
