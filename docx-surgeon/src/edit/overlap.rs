use crate::read::RegionRef;

use super::{Edit, EditError, UnderlineEdit};

/// A plain edit and an underline edit in the *same region* whose spans
/// overlap -- checked separately from each kind's own internal overlap
/// check, since those only ever compare edits of their own kind against
/// each other.
pub(super) fn check_no_cross_kind_overlap(
    edits: &[Edit],
    underline_edits: &[UnderlineEdit],
) -> Result<(), EditError> {
    for edit in edits {
        for underline in underline_edits {
            if edit.region == underline.region
                && underline.flat_start < edit.flat_end
                && edit.flat_start < underline.flat_end
            {
                return Err(EditError::OverlappingEdits {
                    region: edit.region,
                    first: (edit.flat_start, edit.flat_end),
                    second: (underline.flat_start, underline.flat_end),
                });
            }
        }
    }
    Ok(())
}

/// Overlap is only meaningful *within* one region -- two edits in
/// different regions can share identical numeric coordinates without
/// conflicting at all, since those coordinates are relative to
/// different text entirely.
pub(super) fn check_no_overlaps(edits: &[Edit]) -> Result<(), EditError> {
    let spans: Vec<(RegionRef, usize, usize)> = edits
        .iter()
        .map(|e| (e.region, e.flat_start, e.flat_end))
        .collect();
    check_no_overlapping_spans(&spans)
}

/// Shared by [`check_no_overlaps`] and [`UnderlineEdit`]'s own overlap
/// check -- both ultimately just need "no two spans in the same region
/// may overlap," differing only in which struct fields the span's
/// `(start, end)` come from.
pub(super) fn check_no_overlapping_spans(
    spans: &[(RegionRef, usize, usize)],
) -> Result<(), EditError> {
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
