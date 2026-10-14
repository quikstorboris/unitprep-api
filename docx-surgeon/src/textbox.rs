//! Floating text boxes: what the reader records about `<wp:anchor>` +
//! `<wps:txbx>` shapes, and where a value can be written next to one.
//!
//! A form converted from PDF is mostly floating text boxes -- one per
//! field label, each exactly as wide as its label, laid over a drawn
//! field bar. There is no text container for the *value*. Two cases:
//!
//! - **A free-standing box.** The tag goes at the end of the label's own
//!   box, and the box grows to the free space on its row so the value is
//!   not clipped (see [`room_to_right`] and [`crate::edit`]).
//! - **A box inside a group shape** whose drawn "blank" square sits right
//!   after it (an initials box). The tag is written *into that square*,
//!   which becomes a text shape ([`GroupBlank`]).

use quick_xml::events::BytesStart;

/// English Metric Units per typographic point.
pub const EMU_PER_POINT: i64 = 12_700;

/// A box that can grow by less than this is treated as having no room:
/// a tag would not be readable in it anyway.
const MIN_USEFUL_GROWTH: i64 = 20 * EMU_PER_POINT;
/// Gap left between a grown box and the next one on its row.
const NEIGHBOR_GAP: i64 = 2 * EMU_PER_POINT;
/// Two boxes whose vertical offsets differ by less than this are on the
/// same visual row.
const SAME_ROW_TOLERANCE: i64 = 4 * EMU_PER_POINT;
/// How far a group's blank square may overlap its label's box.
const BLANK_OVERLAP_TOLERANCE: i64 = 4 * EMU_PER_POINT;
/// How far to the right of its label a group's blank square may start.
const BLANK_MAX_GAP: i64 = 12 * EMU_PER_POINT;
/// A blank square narrower than this cannot hold a tag.
const BLANK_MIN_WIDTH: i64 = 20 * EMU_PER_POINT;

/// Where a floating text box sits and which bytes hold its width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBoxGeometry {
    /// Byte offset of the body paragraph the box is anchored in. Offsets
    /// are only comparable between boxes sharing an anchor paragraph.
    pub anchor_paragraph: usize,
    pub h_from: String,
    pub h_offset: i64,
    pub v_from: String,
    pub v_offset: i64,
    pub cx: i64,
    pub cy: i64,
    /// Byte range of the `cx` attribute's *value* in `<wp:extent>`.
    pub extent_cx: (usize, usize),
    /// Byte range of the `cx` attribute's *value* in the shape's own
    /// `<a:ext>`. Both must change together.
    pub shape_cx: (usize, usize),
}

/// The drawn blank square of a group shape, right after a label box.
///
/// `insert_at` is the byte offset of the square's `<wps:bodyPr`: a
/// `<wps:txbx>` written there turns the square into a text shape.
/// `run_props` is the label's own first-run `<w:rPr>`, so the tag reads
/// in the form's font and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupBlank {
    pub insert_at: usize,
    pub run_props: Option<(usize, usize)>,
}

/// One floating text box in the document body.
///
/// `text_start..text_end` is the box's text in the body's flat text.
/// `geometry` is `None` for a box inside a group shape (its coordinates
/// are in the group's space, not the page's) -- such a box is never
/// resized; its value goes in `group_blank`, if the group has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextBox {
    pub text_start: usize,
    pub text_end: usize,
    pub geometry: Option<TextBoxGeometry>,
    pub group_blank: Option<GroupBlank>,
}

/// Page width and right margin, for a box with no neighbor to its right.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PageBounds {
    pub width_emu: i64,
    pub right_margin_emu: i64,
}

impl PageBounds {
    pub fn content_right(&self) -> Option<i64> {
        (self.width_emu > 0).then(|| self.width_emu - self.right_margin_emu)
    }
}

/// Free room, in EMU, a box can be grown to -- `None` when it can't be
/// resized at all or there is nothing worth growing into.
pub fn room_to_right(boxes: &[TextBox], index: usize, page: PageBounds) -> Option<i64> {
    let this = boxes[index].geometry.as_ref()?;

    let nearest_neighbor = boxes
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != index)
        .filter_map(|(_, other)| other.geometry.as_ref())
        .filter(|g| {
            g.anchor_paragraph == this.anchor_paragraph
                && g.h_from == this.h_from
                && g.v_from == this.v_from
                && (g.v_offset - this.v_offset).abs() < SAME_ROW_TOLERANCE
                && g.h_offset > this.h_offset + EMU_PER_POINT
        })
        .map(|g| g.h_offset)
        .min();

    let limit = match nearest_neighbor {
        Some(left_edge) => left_edge - NEIGHBOR_GAP,
        // Only a page-relative box has a known right edge to stop at.
        None if this.h_from == "page" => page.content_right()?,
        None => return None,
    };

    let room = limit - this.h_offset;
    (room >= this.cx + MIN_USEFUL_GROWTH).then_some(room)
}

/// The in-progress state of one `<wp:anchor>`.
#[derive(Default)]
struct AnchorBuilder {
    anchor_paragraph: usize,
    h: Option<(String, i64)>,
    v: Option<(String, i64)>,
    extent: Option<(i64, i64, (usize, usize))>,
    shape: Option<(usize, usize)>,
    in_group: bool,
}

/// One child shape (`<wps:wsp>`) of a group, in the group's own
/// coordinate space.
#[derive(Default)]
struct GroupShape {
    x: Option<i64>,
    y: Option<i64>,
    cx: Option<i64>,
    cy: Option<i64>,
    /// Byte offset of this shape's `<wps:bodyPr`.
    body_pr_start: Option<usize>,
    /// Index into the tracker's boxes, if this shape holds text.
    text_box: Option<usize>,
}

impl GroupShape {
    fn rect(&self) -> Option<(i64, i64, i64, i64)> {
        Some((self.x?, self.y?, self.cx?, self.cy?))
    }
}

/// Which axis a `<wp:positionH>`/`<wp:positionV>` element belongs to.
#[derive(Clone, Copy)]
enum Axis {
    Horizontal,
    Vertical,
}

/// Fed the reader's events; collects every [`TextBox`] and the page
/// bounds. All byte offsets are into the original `document.xml`.
#[derive(Default)]
pub(crate) struct TextBoxTracker {
    anchor: Option<AnchorBuilder>,
    axis: Option<(Axis, String)>,
    expecting_offset: bool,
    group_depth: usize,
    in_xfrm: bool,
    content_depth: usize,
    pending_text_start: bool,
    text_start: usize,
    shape: Option<GroupShape>,
    group_shapes: Vec<GroupShape>,
    pub boxes: Vec<TextBox>,
    pub page: PageBounds,
}

impl TextBoxTracker {
    /// `element_start` is the byte offset of the element's own `<`.
    pub fn on_start(
        &mut self,
        name: &[u8],
        e: &BytesStart,
        element_start: usize,
        anchor_paragraph: usize,
    ) {
        match name {
            b"anchor" => {
                self.anchor = Some(AnchorBuilder {
                    anchor_paragraph,
                    ..AnchorBuilder::default()
                });
            }
            b"positionH" => self.axis = Some((Axis::Horizontal, relative_from(e))),
            b"positionV" => self.axis = Some((Axis::Vertical, relative_from(e))),
            b"posOffset" => self.expecting_offset = true,
            b"wgp" | b"grpSp" => {
                self.group_depth += 1;
                if let Some(anchor) = &mut self.anchor {
                    anchor.in_group = true;
                }
            }
            b"wsp" if self.group_depth > 0 => self.shape = Some(GroupShape::default()),
            b"bodyPr" => self.note_body_pr(element_start),
            b"xfrm" => self.in_xfrm = true,
            b"txbxContent" => {
                self.content_depth += 1;
                if self.content_depth == 1 {
                    self.pending_text_start = true;
                }
            }
            _ => {}
        }
    }

    /// The text of `<wp:posOffset>`.
    pub fn on_text(&mut self, text: &str) {
        if !self.expecting_offset {
            return;
        }
        self.expecting_offset = false;
        let (Some((axis, from)), Some(anchor)) = (self.axis.take(), self.anchor.as_mut()) else {
            return;
        };
        let Ok(offset) = text.trim().parse::<i64>() else {
            return;
        };
        match axis {
            Axis::Horizontal => anchor.h = Some((from, offset)),
            Axis::Vertical => anchor.v = Some((from, offset)),
        }
    }

    /// `tag` is the raw text of the self-closing element and `tag_start`
    /// its byte offset in `document.xml`.
    pub fn on_empty(&mut self, name: &[u8], tag: &str, tag_start: usize) {
        match name {
            b"extent" => {
                if let (Some(anchor), Some((cx, cy, range))) =
                    (&mut self.anchor, size_attributes(tag, tag_start))
                {
                    anchor.extent.get_or_insert((cx, cy, range));
                }
            }
            b"ext" if self.in_xfrm && self.group_depth == 0 => {
                if let (Some(anchor), Some((_, _, range))) =
                    (&mut self.anchor, size_attributes(tag, tag_start))
                {
                    anchor.shape.get_or_insert(range);
                }
            }
            b"ext" if self.in_xfrm => {
                if let (Some(shape), Some((cx, cy, _))) =
                    (&mut self.shape, size_attributes(tag, tag_start))
                {
                    shape.cx = Some(cx);
                    shape.cy = Some(cy);
                }
            }
            b"off" if self.in_xfrm => {
                if let Some(shape) = &mut self.shape {
                    shape.x = integer_attribute(tag, "x");
                    shape.y = integer_attribute(tag, "y");
                }
            }
            b"bodyPr" => self.note_body_pr(tag_start),
            b"pgSz" => {
                if let Some(width) = twips_attribute(tag, "w") {
                    self.page.width_emu = width;
                }
            }
            b"pgMar" => {
                if let Some(right) = twips_attribute(tag, "right") {
                    self.page.right_margin_emu = right;
                }
            }
            _ => {}
        }
    }

    pub fn on_end(&mut self, name: &[u8]) {
        match name {
            b"positionH" | b"positionV" => self.axis = None,
            b"wgp" | b"grpSp" => {
                self.group_depth = self.group_depth.saturating_sub(1);
                if self.group_depth == 0 {
                    self.resolve_group_blanks();
                }
            }
            b"wsp" => {
                if let Some(shape) = self.shape.take() {
                    self.group_shapes.push(shape);
                }
            }
            b"xfrm" => self.in_xfrm = false,
            b"anchor" => self.anchor = None,
            _ => {}
        }
    }

    /// Called right after the first paragraph of a `<w:txbxContent>` has
    /// written its separator, with the flat-text length at that point.
    pub fn take_pending_text_start(&mut self, flat_len: usize) {
        if self.pending_text_start {
            self.pending_text_start = false;
            self.text_start = flat_len;
        }
    }

    /// Called at `</w:txbxContent>`. `flat_len` is the body's flat-text
    /// length, or `None` if the box is inside a table cell (not tracked).
    pub fn finish_content(&mut self, flat_len: Option<usize>) {
        self.content_depth = self.content_depth.saturating_sub(1);
        if self.content_depth != 0 {
            return;
        }
        let Some(text_end) = flat_len else {
            return;
        };

        let geometry = self.anchor.as_ref().and_then(|anchor| {
            if anchor.in_group {
                return None;
            }
            let (h_from, h_offset) = anchor.h.clone()?;
            let (v_from, v_offset) = anchor.v.clone()?;
            let (cx, cy, extent_cx) = anchor.extent?;
            Some(TextBoxGeometry {
                anchor_paragraph: anchor.anchor_paragraph,
                h_from,
                h_offset,
                v_from,
                v_offset,
                cx,
                cy,
                extent_cx,
                shape_cx: anchor.shape?,
            })
        });

        if let Some(shape) = &mut self.shape {
            shape.text_box = Some(self.boxes.len());
        }
        self.boxes.push(TextBox {
            text_start: self.text_start,
            text_end,
            geometry,
            group_blank: None,
        });
    }

    fn note_body_pr(&mut self, element_start: usize) {
        if let Some(shape) = &mut self.shape {
            shape.body_pr_start.get_or_insert(element_start);
        }
    }

    /// At the end of a group: for every label box in it, find the drawn
    /// square (a shape with no text of its own) that starts just after
    /// the label, on the same row, and record where a tag can be written.
    fn resolve_group_blanks(&mut self) {
        let shapes = std::mem::take(&mut self.group_shapes);

        for (label_index, label) in shapes.iter().enumerate() {
            let (Some(box_index), Some((lx, ly, lcx, lcy))) = (label.text_box, label.rect()) else {
                continue;
            };

            let blank = shapes
                .iter()
                .enumerate()
                .filter(|&(i, shape)| {
                    i != label_index && shape.text_box.is_none() && shape.body_pr_start.is_some()
                })
                .filter_map(|(_, shape)| Some((shape.rect()?, shape.body_pr_start?)))
                .filter(|&((x, y, cx, cy), _)| {
                    let same_row = ((y + cy / 2) - (ly + lcy / 2)).abs() <= lcy;
                    same_row
                        && x >= lx + lcx - BLANK_OVERLAP_TOLERANCE
                        && x <= lx + lcx + BLANK_MAX_GAP
                        && cx >= BLANK_MIN_WIDTH
                })
                .min_by_key(|&((x, ..), _)| x);

            if let Some((_, insert_at)) = blank {
                self.boxes[box_index].group_blank = Some(GroupBlank {
                    insert_at,
                    run_props: None,
                });
            }
        }
    }
}

fn relative_from(e: &BytesStart) -> String {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == b"relativeFrom")
        .map(|a| String::from_utf8_lossy(&a.value).into_owned())
        .unwrap_or_default()
}

/// The `[start, end)` byte range of the value of attribute `name` in
/// `tag` (the raw text of one element), given `tag`'s own byte offset.
fn attribute_value_range(tag: &str, tag_start: usize, name: &str) -> Option<(usize, usize)> {
    let needle = format!(" {name}=\"");
    let value_start = tag.find(&needle)? + needle.len();
    let value_len = tag[value_start..].find('"')?;
    Some((tag_start + value_start, tag_start + value_start + value_len))
}

fn integer_attribute(tag: &str, name: &str) -> Option<i64> {
    let (start, end) = attribute_value_range(tag, 0, name)?;
    tag[start..end].parse().ok()
}

/// `(cx, cy, byte range of cx's value)` from a `cx`/`cy` element.
fn size_attributes(tag: &str, tag_start: usize) -> Option<(i64, i64, (usize, usize))> {
    let cx_range = attribute_value_range(tag, tag_start, "cx")?;
    let cy_range = attribute_value_range(tag, tag_start, "cy")?;
    let read =
        |(start, end): (usize, usize)| tag[start - tag_start..end - tag_start].parse::<i64>().ok();
    Some((read(cx_range)?, read(cy_range)?, cx_range))
}

/// A `w:*` attribute measured in twentieths of a point, in EMU.
fn twips_attribute(tag: &str, name: &str) -> Option<i64> {
    let (start, end) = attribute_value_range(tag, 0, name)
        .or_else(|| attribute_value_range(tag, 0, &format!("w:{name}")))?;
    Some(tag[start..end].parse::<i64>().ok()? * 635)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry(anchor: usize, h: i64, v: i64, cx: i64) -> TextBoxGeometry {
        TextBoxGeometry {
            anchor_paragraph: anchor,
            h_from: "page".to_string(),
            h_offset: h * EMU_PER_POINT,
            v_from: "paragraph".to_string(),
            v_offset: v * EMU_PER_POINT,
            cx: cx * EMU_PER_POINT,
            cy: 8 * EMU_PER_POINT,
            extent_cx: (0, 0),
            shape_cx: (0, 0),
        }
    }

    fn boxed(geometry: Option<TextBoxGeometry>) -> TextBox {
        TextBox {
            text_start: 0,
            text_end: 0,
            geometry,
            group_blank: None,
        }
    }

    const PAGE: PageBounds = PageBounds {
        width_emu: 612 * EMU_PER_POINT,
        right_margin_emu: 36 * EMU_PER_POINT,
    };

    #[test]
    fn a_box_grows_to_just_short_of_the_next_box_on_its_row() {
        let boxes = [
            boxed(Some(geometry(1, 47, 0, 78))),
            boxed(Some(geometry(1, 220, 0, 16))),
        ];

        assert_eq!(
            room_to_right(&boxes, 0, PAGE),
            Some((220 - 2 - 47) * EMU_PER_POINT)
        );
    }

    #[test]
    fn a_box_on_another_row_or_paragraph_is_not_a_neighbor() {
        let boxes = [
            boxed(Some(geometry(1, 47, 0, 78))),
            boxed(Some(geometry(1, 220, 18, 16))),
            boxed(Some(geometry(2, 220, 0, 16))),
        ];

        assert_eq!(
            room_to_right(&boxes, 0, PAGE),
            Some((576 - 47) * EMU_PER_POINT)
        );
    }

    #[test]
    fn the_last_box_on_a_row_grows_to_the_right_margin() {
        let boxes = [boxed(Some(geometry(1, 515, 18, 13)))];

        assert_eq!(
            room_to_right(&boxes, 0, PAGE),
            Some((576 - 515) * EMU_PER_POINT)
        );
    }

    #[test]
    fn a_box_with_no_room_or_no_geometry_cannot_grow() {
        let cramped = [
            boxed(Some(geometry(1, 47, 0, 78))),
            boxed(Some(geometry(1, 130, 0, 16))),
        ];
        let grouped = [boxed(None)];

        assert_eq!(room_to_right(&cramped, 0, PAGE), None);
        assert_eq!(room_to_right(&grouped, 0, PAGE), None);
    }

    #[test]
    fn an_unknown_page_width_means_the_last_box_cannot_grow() {
        let boxes = [boxed(Some(geometry(1, 515, 18, 13)))];

        assert_eq!(room_to_right(&boxes, 0, PageBounds::default()), None);
    }

    #[test]
    fn attribute_ranges_point_at_the_value_bytes() {
        let tag = r#"<wp:extent cx="985520" cy="106680"/>"#;
        let (cx, cy, (start, end)) = size_attributes(tag, 100).unwrap();

        assert_eq!((cx, cy), (985_520, 106_680));
        assert_eq!(&tag[start - 100..end - 100], "985520");
    }
}
