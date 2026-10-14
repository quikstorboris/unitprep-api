//! What the tagging UI shows for each candidate: regions, confidence tiers and context snippets.

use docx_surgeon::RegionRef;
use serde::{Deserialize, Serialize};
use unitprep_tagger_pipeline::{ConfidenceTier, RegionCandidate};

/// How much surrounding text a candidate's snippet carries on each side
/// -- enough to read the label/context around a match without sending
/// the whole region back to the browser.
pub(super) const SNIPPET_CONTEXT_CHARS: usize = 30;

/// Hard ceiling on how many candidates one `/tagger/check` run will
/// process past `find_candidates` -- a real template's candidate count is
/// "tens, not thousands" per `assign_tiers`'s own doc comment; this bounds
/// a pathological or adversarial document (e.g. a blank repeated
/// thousands of times) well above any real template but far below where
/// building candidate views, cloning matched text, and storing the
/// session would become its own resource concern.
pub(super) const MAX_CANDIDATES: usize = 2000;

#[derive(Debug, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum RegionView {
    Body,
    TableCell { index: usize },
}

impl From<RegionRef> for RegionView {
    fn from(region: RegionRef) -> Self {
        match region {
            RegionRef::Body => RegionView::Body,
            RegionRef::TableCell(index) => RegionView::TableCell { index },
        }
    }
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TierView {
    Auto,
    NeedsReview,
}

impl From<ConfidenceTier> for TierView {
    fn from(tier: ConfidenceTier) -> Self {
        match tier {
            ConfidenceTier::Auto => TierView::Auto,
            ConfidenceTier::NeedsReview => TierView::NeedsReview,
        }
    }
}

/// One candidate as the review UI sees it. `index` is this candidate's
/// position in the session's own candidate list -- `/tagger/apply`
/// references a candidate by this same index, not by re-sending its
/// coordinates.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CandidateView {
    pub index: usize,
    pub region: RegionView,
    pub tag_key: String,
    pub matched_text: String,
    pub tier: TierView,
    pub snippet: String,
}

pub(super) fn build_candidate_views(
    doc: &docx_surgeon::FlatDocument,
    candidates: &[RegionCandidate],
) -> Vec<CandidateView> {
    candidates
        .iter()
        .enumerate()
        .map(|(index, rc)| {
            let region_text = match rc.region {
                RegionRef::Body => &doc.body.text,
                RegionRef::TableCell(i) => &doc.table_cells[i].text,
            };
            CandidateView {
                index,
                region: rc.region.into(),
                tag_key: rc.candidate.tag_key.clone(),
                matched_text: visible_matched_text(&rc.candidate.matched_text),
                tier: rc.tier.into(),
                snippet: snippet_for(doc, rc, region_text),
            }
        })
        .collect()
}

const TAB_MARKER: char = '\u{21e5}';

/// A tab is invisible in a review list, so an underlined-tab blank's
/// `matched_text` is shown as `⇥`.
fn visible_matched_text(matched_text: &str) -> String {
    if matched_text == unitprep_template_tagger::TAB_BLANK_TEXT {
        TAB_MARKER.to_string()
    } else {
        matched_text.to_string()
    }
}

/// The context excerpt for one candidate.
///
/// - An empty table cell has no text of its own to excerpt, so its
///   snippet is the cell before it (the label) plus a marker for the
///   empty cell itself.
/// - An underlined-tab blank marks its own tab with `⇥`; every other tab
///   in the excerpt becomes a plain space (same byte length, so the
///   candidate's offsets still line up), keeping the marker unique for
///   the UI's highlight.
fn snippet_for(
    doc: &docx_surgeon::FlatDocument,
    rc: &RegionCandidate,
    region_text: &str,
) -> String {
    if let RegionRef::TableCell(index) = rc.region {
        if region_text.is_empty() && index > 0 {
            let label_cell = &doc.table_cells[index - 1].text;
            let from = char_boundary_at_or_after(
                label_cell,
                label_cell.len().saturating_sub(2 * SNIPPET_CONTEXT_CHARS),
            );
            return format!("{} [empty cell]", &label_cell[from..]);
        }
    }

    // A text-box label's value is appended at the end of the box: show
    // the label (and the labels just before it, to say which section)
    // followed by a marker for where the value goes.
    if rc.region == RegionRef::Body
        && rc.candidate.start == rc.candidate.end
        && rc.candidate.matched_text.is_empty()
    {
        let end = rc.candidate.end;
        let from =
            char_boundary_at_or_after(region_text, end.saturating_sub(2 * SNIPPET_CONTEXT_CHARS));
        let lines: Vec<&str> = region_text[from..end]
            .split('\n')
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        return format!("{} [value goes here]", lines.join(" / "));
    }

    if rc.candidate.matched_text == unitprep_template_tagger::TAB_BLANK_TEXT {
        let spaced: String = region_text
            .char_indices()
            .map(|(i, c)| {
                if c == '\t' && i != rc.candidate.start {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        return build_snippet(&spaced, rc.candidate.start, rc.candidate.end)
            .replace('\t', &TAB_MARKER.to_string());
    }

    build_snippet(region_text, rc.candidate.start, rc.candidate.end)
}

pub(super) fn char_boundary_at_or_after(text: &str, mut idx: usize) -> usize {
    while idx < text.len() && !text.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

pub(super) fn char_boundary_at_or_before(text: &str, mut idx: usize) -> usize {
    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

pub(super) fn build_snippet(text: &str, start: usize, end: usize) -> String {
    let snippet_start =
        char_boundary_at_or_after(text, start.saturating_sub(SNIPPET_CONTEXT_CHARS));
    let snippet_end =
        char_boundary_at_or_before(text, (end + SNIPPET_CONTEXT_CHARS).min(text.len()));

    let mut snippet = String::new();
    if snippet_start > 0 {
        snippet.push('\u{2026}');
    }
    snippet.push_str(&text[snippet_start..snippet_end]);
    if snippet_end < text.len() {
        snippet.push('\u{2026}');
    }
    snippet
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct TaggerCheckResponse {
    pub session_id: String,
    pub candidates: Vec<CandidateView>,
}

#[derive(Debug, Deserialize)]
pub struct TaggerSessionRequest {
    pub session_id: String,
}
