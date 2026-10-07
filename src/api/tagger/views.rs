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

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Serialize)]
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
                matched_text: rc.candidate.matched_text.clone(),
                tier: rc.tier.into(),
                snippet: build_snippet(region_text, rc.candidate.start, rc.candidate.end),
            }
        })
        .collect()
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

#[derive(Debug, Serialize)]
pub struct TaggerCheckResponse {
    pub session_id: String,
    pub candidates: Vec<CandidateView>,
}

#[derive(Debug, Deserialize)]
pub struct TaggerSessionRequest {
    pub session_id: String,
}
