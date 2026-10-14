use crate::read::{FlatDocument, RegionRef};

use super::run_xml::xml_escape_text;
use super::Edit;

/// The group blank an edit writes into, if the edit is a zero-width
/// append at the end of a label box whose group shape has one.
pub(super) fn group_blank_target(doc: &FlatDocument, edit: &Edit) -> bool {
    edit.region == RegionRef::Body
        && edit.flat_start == edit.flat_end
        && doc.group_blank_ending_at(edit.flat_start).is_some()
}

/// Turns the drawn blank square after a label into a text shape holding
/// the tag: a `<wps:txbx>` written just before the square's `<wps:bodyPr`.
/// The label box's own text is not touched.
///
/// `edit` must satisfy [`group_blank_target`].
pub(super) fn group_blank_replacement(
    document_xml: &str,
    doc: &FlatDocument,
    edit: &Edit,
) -> (usize, usize, String) {
    let blank = doc
        .group_blank_ending_at(edit.flat_start)
        .expect("checked by group_blank_target");

    // `<wps:bodyPr`: the namespace prefix the document itself uses.
    let prefix = document_xml[blank.insert_at + 1..]
        .split(':')
        .next()
        .unwrap_or("wps");
    let run_props = blank
        .run_props
        .map(|(start, end)| &document_xml[start..end])
        .unwrap_or("");
    let tag = xml_escape_text(edit.replacement.trim());

    let content = format!(
        concat!(
            "<{p}:txbx><w:txbxContent><w:p><w:pPr><w:jc w:val=\"center\"/></w:pPr>",
            "<w:r>{rpr}<w:t xml:space=\"preserve\">{tag}</w:t></w:r></w:p></w:txbxContent></{p}:txbx>"
        ),
        p = prefix,
        rpr = run_props,
        tag = tag
    );
    (blank.insert_at, blank.insert_at, content)
}

/// Byte-range replacements that widen the text box an edit appends to.
///
/// A tag written at the very end of a floating text box that was sized
/// to its own label would wrap or be clipped, so such an edit also grows
/// the box to the free room on its row ([`FlatDocument::
/// textbox_room_emu`]). Both widths the file keeps for a shape --
/// `<wp:extent>` and the shape's own `<a:ext>` -- change together. A box
/// with no room, or inside a group shape, is left alone.
pub(super) fn textbox_growth_replacements(
    doc: &FlatDocument,
    edits: &[Edit],
) -> Vec<(usize, usize, String)> {
    let mut grown: Vec<usize> = Vec::new();
    let mut replacements = Vec::new();

    for edit in edits {
        if edit.region != RegionRef::Body || edit.flat_start != edit.flat_end {
            continue;
        }
        let Some(index) = doc.textbox_ending_at(edit.flat_start) else {
            continue;
        };
        if grown.contains(&index) {
            continue;
        }
        grown.push(index);

        let (Some(room), Some(geometry)) = (
            doc.textbox_room_emu(index),
            doc.textboxes[index].geometry.as_ref(),
        ) else {
            continue;
        };
        replacements.push((geometry.extent_cx.0, geometry.extent_cx.1, room.to_string()));
        replacements.push((geometry.shape_cx.0, geometry.shape_cx.1, room.to_string()));
    }

    replacements
}
