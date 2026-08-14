use printpdf::{Px, RawImage, XObjectTransform};

// Landscape, not portrait -- a 6-column table needs the width more than
// it needs the height, and a wide tabular report being landscape is a
// completely standard convention (bank/financial statements, spreadsheet
// exports), not a formality downgrade. Switched after a real render
// showed Event and Details truncating hard in portrait's narrower 180mm
// usable width; landscape's 267mm gives both columns real room without
// needing a text-measuring layout engine.
pub(super) const PAGE_WIDTH_MM: f32 = 297.0;
pub(super) const PAGE_HEIGHT_MM: f32 = 210.0;
pub(super) const LEFT_MARGIN_MM: f32 = 15.0;
pub(super) const TOP_START_MM: f32 = 190.0;
pub(super) const BOTTOM_MARGIN_MM: f32 = 20.0;
pub(super) const ROW_HEIGHT_MM: f32 = 6.0;
pub(super) const HEADER_FONT_SIZE: f32 = 9.0;
pub(super) const BODY_FONT_SIZE: f32 = 8.0;
pub(super) const TITLE_FONT_SIZE: f32 = 16.0;
pub(super) const META_FONT_SIZE: f32 = 10.0;

// Details wraps across multiple lines rather than truncating -- it's the
// one column carrying genuinely prose-like content (before/after diff
// summaries, metadata), unlike a name or IP address where truncation
// loses nothing worth keeping. Capped at 3 lines so a pathological long
// value (a large metadata dump) can't blow up a single row's height
// unboundedly; anything past the cap is marked, not silently dropped.
// Continuation lines use a tighter line height than ROW_HEIGHT_MM --
// they're wrapped lines within one logical row, not new rows.
pub(super) const DETAILS_MAX_WRAP_LINES: usize = 3;
pub(super) const DETAIL_LINE_HEIGHT_MM: f32 = 4.0;

// Letterhead logo -- page 1 only, top-left. Compiled into the binary
// (not a runtime file path) so the export has no filesystem dependency
// beyond what's already true of the built-in fonts.
pub(super) static LOGO_PNG_BYTES: &[u8] =
    include_bytes!("../../../assets/pdf/orchestrator-logo-light.png");
pub(super) const LOGO_WIDTH_MM: f32 = 40.0;
// 20mm down from the page's top edge -- widened from an initial 13mm
// after a real render looked too tight against the top of the page.
const LOGO_BOTTOM_Y_MM: f32 = PAGE_HEIGHT_MM - 20.0;
// Fixed 8mm below the logo's bottom edge, not derived from its height --
// robust to the logo's aspect ratio changing later without the title
// ever risking an overlap.
pub(super) const TITLE_Y_MM: f32 = LOGO_BOTTOM_Y_MM - 8.0;

pub(super) fn mm_to_pt(mm: f32) -> f32 {
    mm * 72.0 / 25.4
}

/// Scales the logo to `LOGO_WIDTH_MM` wide (uniformly, so its aspect ratio
/// is preserved regardless of the source image's own dimensions) and
/// places its bottom-left corner at the left margin, `LOGO_BOTTOM_Y_MM`
/// up from the page's bottom edge.
pub(super) fn logo_transform(image: &RawImage) -> XObjectTransform {
    const DPI: f32 = 300.0;
    let native_width_pt = Px(image.width).into_pt(DPI).0;
    let scale = mm_to_pt(LOGO_WIDTH_MM) / native_width_pt;

    XObjectTransform {
        translate_x: Some(printpdf::Pt(mm_to_pt(LEFT_MARGIN_MM))),
        translate_y: Some(printpdf::Pt(mm_to_pt(LOGO_BOTTOM_Y_MM))),
        scale_x: Some(scale),
        scale_y: Some(scale),
        dpi: Some(DPI),
        ..Default::default()
    }
}

pub(super) struct Column {
    pub(super) label: &'static str,
    /// Offset from `LEFT_MARGIN_MM`, not an absolute page position -- so
    /// the whole table can move if the margin ever changes without every
    /// column needing its own edit.
    pub(super) x_offset_mm: f32,
    pub(super) max_chars: usize,
}

// Widths (the gap between one column's x_offset_mm and the next's) use a
// conservative ~1.8mm/char plus a couple mm of unused margin baked into
// each max_chars -- verified against a real render (see
// write_sample_pdf_to_disk_for_manual_inspection).
//
// Actor/Target/IP were sized down from an earlier pass that budgeted for
// worst-case content (a bare 36-character UUID fallback, or a full IPv4
// literal) on every row -- but a *resolved* actor/target is typically
// "First Last" (well under half that), so the wider budget just left
// visible dead space in front of IP and Details on a typical row. Actor/
// Target still have enough room for most real names; the rare
// unresolved-UUID fallback truncates harder now, which is an acceptable
// trade since the full UUID remains visible on the on-screen audit log
// page regardless. The reclaimed width goes to Details -- max_chars here
// is a *per-line* budget now that Details wraps (see DETAILS_MAX_WRAP_LINES
// above) rather than a single-line truncation limit.
pub(super) const COLUMNS: &[Column] = &[
    Column {
        label: "Time",
        x_offset_mm: 0.0,
        max_chars: 16,
    },
    Column {
        label: "Event",
        x_offset_mm: 32.0,
        max_chars: 26,
    },
    Column {
        label: "Actor",
        x_offset_mm: 82.0,
        max_chars: 16,
    },
    Column {
        label: "Target",
        x_offset_mm: 114.0,
        max_chars: 16,
    },
    Column {
        label: "IP",
        x_offset_mm: 146.0,
        // 15 chars fits the longest possible IPv4 literal
        // ("255.255.255.255") without truncation.
        max_chars: 15,
    },
    Column {
        label: "Details",
        x_offset_mm: 175.0,
        max_chars: 50,
    },
];

/// The Details column's own `Column` -- pulled out once since both
/// `data_row_ops` and `row_height_mm` need it and `COLUMNS` is always
/// non-empty (Details is always the last one).
pub(super) fn details_column() -> &'static Column {
    COLUMNS.last().expect("COLUMNS is never empty")
}

/// Truncates to `max_chars`, appending an ellipsis when it actually cut
/// something -- so a shortened cell is visually distinguishable from one
/// that just happened to be exactly at the limit.
pub(super) fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut shortened: String = value.chars().take(max_chars.saturating_sub(1)).collect();
    shortened.push('…');
    shortened
}

/// Greedily word-wraps `value` into at most `max_lines` lines of at most
/// `max_chars` each. Only ever called for the Details column -- every
/// other cell stays single-line-and-truncated (a name or an IP address
/// truncated mid-way loses nothing worth keeping; a before/after diff
/// summary cut off is a genuinely different, worse loss).
///
/// A single word longer than `max_chars` is truncated in place rather
/// than left to overflow the column (rare -- Details content is short
/// structured text in practice, not long unbroken tokens). If more words
/// remain than `max_lines` could hold, the last line gets an ellipsis
/// appended (or is truncated to make room for one) so a capped value
/// reads as "there's more", not as a complete value that happened to be
/// short.
pub(super) fn wrap_lines(value: &str, max_chars: usize, max_lines: usize) -> Vec<String> {
    let words: Vec<&str> = value.split_whitespace().collect();
    if words.is_empty() || max_lines == 0 {
        return Vec::new();
    }

    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut index = 0;

    while index < words.len() && lines.len() < max_lines {
        let word = words[index];

        if current.is_empty() {
            current = truncate(word, max_chars);
            index += 1;
            continue;
        }

        let candidate = format!("{current} {word}");
        if candidate.chars().count() <= max_chars {
            current = candidate;
            index += 1;
        } else {
            lines.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }

    if index < words.len() {
        if let Some(last) = lines.last_mut() {
            *last = if last.chars().count() < max_chars {
                format!("{last}…")
            } else {
                truncate(last, max_chars)
            };
        }
    }

    lines
}

/// A row's total height -- `ROW_HEIGHT_MM` for an empty or single-line
/// Details value, plus one `DETAIL_LINE_HEIGHT_MM` per extra wrapped
/// line. Rows are no longer uniform height once Details can wrap, so
/// pagination (`paginate_rows` below) has to ask each row for its own.
pub(super) fn row_height_mm(row: &super::AuditLogPdfRow) -> f32 {
    let details_column = details_column();
    let line_count = wrap_lines(
        &row.details,
        details_column.max_chars,
        DETAILS_MAX_WRAP_LINES,
    )
    .len()
    .max(1);
    ROW_HEIGHT_MM + (line_count - 1) as f32 * DETAIL_LINE_HEIGHT_MM
}

/// Greedy-packs `rows` into pages by each row's own height (see
/// `row_height_mm`) rather than a fixed row count per page -- a fixed
/// count can't work once rows have variable height. `first_page_budget_mm`
/// is the vertical space available for rows on page 1 (smaller, since it
/// also carries the title/metadata block); every later page gets
/// `later_page_budget_mm`. Always places at least one row per page even
/// if that row alone exceeds the budget, so a single oversized wrapped
/// row can't produce an infinite loop or a silently empty page.
pub(super) fn paginate_rows(
    rows: &[super::AuditLogPdfRow],
    first_page_budget_mm: f32,
    later_page_budget_mm: f32,
) -> Vec<&[super::AuditLogPdfRow]> {
    let mut pages = Vec::new();
    let mut start = 0;
    let mut budget = first_page_budget_mm;

    while start < rows.len() {
        let mut used_mm = 0.0;
        let mut end = start;

        while end < rows.len() {
            let height = row_height_mm(&rows[end]);
            if end > start && used_mm + height > budget {
                break;
            }
            used_mm += height;
            end += 1;
        }

        pages.push(&rows[start..end]);
        start = end;
        budget = later_page_budget_mm;
    }

    pages
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::audit_log_pdf::AuditLogPdfRow;

    fn sample_row(n: usize) -> AuditLogPdfRow {
        // Same "%Y-%m-%d %H:%M" format the real caller
        // (api::auth_audit_logs::export_audit_logs) produces -- computed
        // via real date rollover (chrono::Duration) rather than a hand-
        // written string, so this fixture can't drift into a plausible-
        // looking but invalid date ("2026-08-010") the way a naive
        // format!("...-0{n}...") would past single digits.
        let created_at = chrono::DateTime::parse_from_rfc3339("2026-08-01T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
            + chrono::Duration::hours(n as i64);

        // Alternates between a bare occurrence and a change-type event so
        // a real render exercises both the empty-Details case and a
        // realistic before/after diff summary in the same sample.
        let (event_type, details) = if n.is_multiple_of(2) {
            (
                "user_deactivated".to_string(),
                "status: active -> deactivated".to_string(),
            )
        } else {
            ("login_succeeded".to_string(), String::new())
        };

        AuditLogPdfRow {
            created_at: created_at.format("%Y-%m-%d %H:%M").to_string(),
            event_type,
            actor_label: "Ada Lovelace (ada@example.com)".to_string(),
            target_label: "—".to_string(),
            ip_address: "203.0.113.1".to_string(),
            details,
        }
    }

    #[test]
    fn truncate_leaves_short_values_untouched() {
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn truncate_shortens_and_marks_long_values() {
        let result = truncate("this is a long value", 10);
        assert_eq!(result.chars().count(), 10);
        assert!(result.ends_with('…'));
    }

    #[test]
    fn wrap_lines_returns_nothing_for_empty_input() {
        assert_eq!(wrap_lines("", 10, 3), Vec::<String>::new());
        assert_eq!(wrap_lines("   ", 10, 3), Vec::<String>::new());
    }

    #[test]
    fn wrap_lines_keeps_a_short_value_on_one_line() {
        assert_eq!(wrap_lines("status: active", 30, 3), vec!["status: active"]);
    }

    #[test]
    fn wrap_lines_wraps_across_multiple_lines() {
        let result = wrap_lines("status: active -> deactivated", 12, 3);
        assert_eq!(result, vec!["status:", "active ->", "deactivated"]);
    }

    #[test]
    fn wrap_lines_caps_at_max_lines_and_marks_truncation() {
        let result = wrap_lines("one two three four five six", 4, 2);
        assert_eq!(result.len(), 2);
        assert!(result[1].ends_with('…'));
    }

    #[test]
    fn wrap_lines_truncates_a_single_word_longer_than_max_chars() {
        let result = wrap_lines("supercalifragilisticexpialidocious", 10, 3);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].chars().count(), 10);
        assert!(result[0].ends_with('…'));
    }

    #[test]
    fn row_height_matches_row_height_mm_for_empty_details() {
        let row = sample_row(1); // odd n -> empty details, per sample_row
        assert_eq!(row_height_mm(&row), ROW_HEIGHT_MM);
    }

    #[test]
    fn row_height_grows_for_wrapped_details() {
        let mut row = sample_row(2); // even n -> non-empty details
        row.details =
            "one two three four five six seven eight nine ten eleven twelve thirteen".to_string();
        let details_column = details_column();
        let line_count = wrap_lines(
            &row.details,
            details_column.max_chars,
            DETAILS_MAX_WRAP_LINES,
        )
        .len();
        assert!(line_count > 1);
        assert_eq!(
            row_height_mm(&row),
            ROW_HEIGHT_MM + (line_count - 1) as f32 * DETAIL_LINE_HEIGHT_MM
        );
    }

    #[test]
    fn paginate_rows_always_makes_progress_even_over_budget() {
        let rows: Vec<AuditLogPdfRow> = (1..=5).map(sample_row).collect();
        // A budget smaller than even one row's height must still place
        // exactly one row per page, not loop forever or drop rows.
        let pages = paginate_rows(&rows, 0.0, 0.0);
        let total: usize = pages.iter().map(|page| page.len()).sum();
        assert_eq!(total, rows.len());
        assert!(pages.iter().all(|page| page.len() == 1));
    }

    #[test]
    fn paginate_rows_packs_multiple_rows_per_page_when_budget_allows() {
        let rows: Vec<AuditLogPdfRow> = (1..=5).map(sample_row).collect();
        let pages = paginate_rows(&rows, 100.0, 100.0);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].len(), 5);
    }
}
