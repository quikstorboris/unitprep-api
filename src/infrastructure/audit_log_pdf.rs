//! Renders the admin audit-log export as a PDF, entirely in memory.
//!
//! Uses `printpdf`'s built-in Helvetica/Helvetica-Bold fonts (one of the
//! 14 standard PDF fonts every viewer already has) rather than embedding a
//! TTF file -- zero extra assets to ship with the binary, same
//! self-hosted-library-with-minimal-footprint preference as the rest of
//! this project. `printpdf` itself is pulled in with `default-features =
//! false` (see Cargo.toml) specifically to avoid its default HTML/CSS
//! layout engine (`azul`), which this module has no use for: a fixed-
//! column table with manual pagination needs none of that.
//!
//! Column positions/widths and per-cell character-count limits below are
//! a rough heuristic (Helvetica's average glyph width at the chosen font
//! size), not a text-measuring layout engine -- good enough for an
//! internal admin report, where a slightly conservative truncation is a
//! non-issue and a real layout engine would be a lot of dependency for
//! very little benefit.

use printpdf::{
    BuiltinFont, Color, Mm, Op, PdfDocument, PdfFontHandle, PdfPage, PdfSaveOptions, Point, Pt, Px,
    RawImage, Rgb, TextItem, TextMatrix, XObjectTransform,
};

// Landscape, not portrait -- a 6-column table needs the width more than
// it needs the height, and a wide tabular report being landscape is a
// completely standard convention (bank/financial statements, spreadsheet
// exports), not a formality downgrade. Switched after a real render
// showed Event and Details truncating hard in portrait's narrower 180mm
// usable width; landscape's 267mm gives both columns real room without
// needing a text-measuring layout engine.
const PAGE_WIDTH_MM: f32 = 297.0;
const PAGE_HEIGHT_MM: f32 = 210.0;
const LEFT_MARGIN_MM: f32 = 15.0;
const TOP_START_MM: f32 = 190.0;
const BOTTOM_MARGIN_MM: f32 = 20.0;
const ROW_HEIGHT_MM: f32 = 6.0;
const HEADER_FONT_SIZE: f32 = 9.0;
const BODY_FONT_SIZE: f32 = 8.0;
const TITLE_FONT_SIZE: f32 = 16.0;
const META_FONT_SIZE: f32 = 10.0;

// Details wraps across multiple lines rather than truncating -- it's the
// one column carrying genuinely prose-like content (before/after diff
// summaries, metadata), unlike a name or IP address where truncation
// loses nothing worth keeping. Capped at 3 lines so a pathological long
// value (a large metadata dump) can't blow up a single row's height
// unboundedly; anything past the cap is marked, not silently dropped.
// Continuation lines use a tighter line height than ROW_HEIGHT_MM --
// they're wrapped lines within one logical row, not new rows.
const DETAILS_MAX_WRAP_LINES: usize = 3;
const DETAIL_LINE_HEIGHT_MM: f32 = 4.0;

// Letterhead logo -- page 1 only, top-left. Compiled into the binary
// (not a runtime file path) so the export has no filesystem dependency
// beyond what's already true of the built-in fonts.
static LOGO_PNG_BYTES: &[u8] = include_bytes!("../../assets/pdf/orchestrator-logo-light.png");
const LOGO_WIDTH_MM: f32 = 40.0;
// 20mm down from the page's top edge -- widened from an initial 13mm
// after a real render looked too tight against the top of the page.
const LOGO_BOTTOM_Y_MM: f32 = PAGE_HEIGHT_MM - 20.0;
// Fixed 8mm below the logo's bottom edge, not derived from its height --
// robust to the logo's aspect ratio changing later without the title
// ever risking an overlap.
const TITLE_Y_MM: f32 = LOGO_BOTTOM_Y_MM - 8.0;

fn mm_to_pt(mm: f32) -> f32 {
    mm * 72.0 / 25.4
}

/// Scales the logo to `LOGO_WIDTH_MM` wide (uniformly, so its aspect ratio
/// is preserved regardless of the source image's own dimensions) and
/// places its bottom-left corner at the left margin, `LOGO_BOTTOM_Y_MM`
/// up from the page's bottom edge.
fn logo_transform(image: &RawImage) -> XObjectTransform {
    const DPI: f32 = 300.0;
    let native_width_pt = Px(image.width).into_pt(DPI).0;
    let scale = mm_to_pt(LOGO_WIDTH_MM) / native_width_pt;

    XObjectTransform {
        translate_x: Some(Pt(mm_to_pt(LEFT_MARGIN_MM))),
        translate_y: Some(Pt(mm_to_pt(LOGO_BOTTOM_Y_MM))),
        scale_x: Some(scale),
        scale_y: Some(scale),
        dpi: Some(DPI),
        ..Default::default()
    }
}

struct Column {
    label: &'static str,
    /// Offset from `LEFT_MARGIN_MM`, not an absolute page position -- so
    /// the whole table can move if the margin ever changes without every
    /// column needing its own edit.
    x_offset_mm: f32,
    max_chars: usize,
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
const COLUMNS: &[Column] = &[
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

pub struct AuditLogPdfRow {
    pub created_at: String,
    pub event_type: String,
    pub actor_label: String,
    pub target_label: String,
    pub ip_address: String,
    pub details: String,
}

impl AuditLogPdfRow {
    /// Every column except Details, which wraps onto multiple lines and
    /// so isn't rendered through the single-line generic path the rest
    /// of the row uses.
    fn single_line_cells(&self) -> [&str; 5] {
        [
            &self.created_at,
            &self.event_type,
            &self.actor_label,
            &self.target_label,
            &self.ip_address,
        ]
    }
}

pub struct AuditLogPdfReport {
    pub generated_by: String,
    pub generated_at: String,
    /// Pre-formatted lines, one per filter -- kept as plain strings rather
    /// than a structured filter type, since this module's only job is
    /// laying text on a page, not deciding how a filter should read.
    pub filter_lines: Vec<String>,
    pub rows: Vec<AuditLogPdfRow>,
    /// Set when the result set was capped -- (rows shown, the cap itself),
    /// rendered as a closing note rather than silently returning a partial
    /// export with no indication anything was left out.
    pub truncated_at: Option<usize>,
}

fn black() -> Color {
    Color::Rgb(Rgb {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        icc_profile: None,
    })
}

fn gray() -> Color {
    Color::Rgb(Rgb {
        r: 0.4,
        g: 0.4,
        b: 0.4,
        icc_profile: None,
    })
}

/// Truncates to `max_chars`, appending an ellipsis when it actually cut
/// something -- so a shortened cell is visually distinguishable from one
/// that just happened to be exactly at the limit.
fn truncate(value: &str, max_chars: usize) -> String {
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
fn wrap_lines(value: &str, max_chars: usize, max_lines: usize) -> Vec<String> {
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

/// Places one piece of text at an absolute page position.
///
/// Deliberately `Op::SetTextMatrix { matrix: TextMatrix::Translate(..) }`,
/// not `Op::SetTextCursor`. `SetTextCursor` compiles to the raw PDF `Td`
/// operator, which moves *relative to the current line's start* -- not
/// absolute, despite what the field name suggests. A grid layout that
/// calls it repeatedly within one text section (one per cell) accumulates
/// every previous move on top of the next one, so only the very first
/// cell in a row lands where intended and everything after it drifts off
/// the visible page. `Tm` (what `SetTextMatrix` emits) replaces the text
/// matrix outright, so each call here is genuinely absolute regardless of
/// how many text draws came before it in the same section.
fn show_text_at(x_mm: f32, y_mm: f32, value: String) -> Vec<Op> {
    let point = Point::new(Mm(x_mm), Mm(y_mm));
    vec![
        Op::SetTextMatrix {
            matrix: TextMatrix::Translate(point.x, point.y),
        },
        Op::ShowText {
            items: vec![TextItem::Text(value)],
        },
    ]
}

fn set_font(font: BuiltinFont, size: f32, color: Color) -> Vec<Op> {
    vec![
        Op::SetFillColor { col: color },
        Op::SetFont {
            font: PdfFontHandle::Builtin(font),
            size: Pt(size),
        },
        Op::SetLineHeight { lh: Pt(size) },
    ]
}

fn header_row_ops(y_mm: f32) -> Vec<Op> {
    let mut ops = set_font(BuiltinFont::HelveticaBold, HEADER_FONT_SIZE, black());
    for column in COLUMNS {
        ops.extend(show_text_at(
            LEFT_MARGIN_MM + column.x_offset_mm,
            y_mm,
            column.label.to_string(),
        ));
    }
    ops
}

/// The Details column's own `Column` -- pulled out once since both
/// `data_row_ops` and `row_height_mm` need it and `COLUMNS` is always
/// non-empty (Details is always the last one).
fn details_column() -> &'static Column {
    COLUMNS.last().expect("COLUMNS is never empty")
}

fn data_row_ops(row: &AuditLogPdfRow, top_y_mm: f32) -> Vec<Op> {
    let mut ops = Vec::new();

    for (column, value) in COLUMNS[..COLUMNS.len() - 1]
        .iter()
        .zip(row.single_line_cells())
    {
        ops.extend(show_text_at(
            LEFT_MARGIN_MM + column.x_offset_mm,
            top_y_mm,
            truncate(value, column.max_chars),
        ));
    }

    let details_column = details_column();
    let wrapped = wrap_lines(
        &row.details,
        details_column.max_chars,
        DETAILS_MAX_WRAP_LINES,
    );
    for (index, line) in wrapped.into_iter().enumerate() {
        ops.extend(show_text_at(
            LEFT_MARGIN_MM + details_column.x_offset_mm,
            top_y_mm - (index as f32) * DETAIL_LINE_HEIGHT_MM,
            line,
        ));
    }

    ops
}

/// A row's total height -- `ROW_HEIGHT_MM` for an empty or single-line
/// Details value, plus one `DETAIL_LINE_HEIGHT_MM` per extra wrapped
/// line. Rows are no longer uniform height once Details can wrap, so
/// pagination (`paginate_rows` below) has to ask each row for its own.
fn row_height_mm(row: &AuditLogPdfRow) -> f32 {
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
fn paginate_rows(
    rows: &[AuditLogPdfRow],
    first_page_budget_mm: f32,
    later_page_budget_mm: f32,
) -> Vec<&[AuditLogPdfRow]> {
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

/// Renders the full report to PDF bytes, paginating the row list across
/// as many pages as needed.
pub fn render_audit_log_pdf(report: &AuditLogPdfReport) -> Vec<u8> {
    let mut doc = PdfDocument::new("UnitPrep Audit Log Report");

    let logo_image = RawImage::decode_from_bytes(LOGO_PNG_BYTES, &mut Vec::new())
        .expect("bundled logo PNG must decode -- it's compiled into the binary, not user input");
    let logo_transform = logo_transform(&logo_image);
    let logo_id = doc.add_image(&logo_image);

    let mut pages = Vec::new();

    // Page 1 carries the letterhead logo and the title/metadata block, so
    // its table starts lower than every subsequent page's.
    let mut first_page_ops = vec![
        Op::SaveGraphicsState,
        Op::UseXobject {
            id: logo_id,
            transform: logo_transform,
        },
        Op::StartTextSection,
    ];
    first_page_ops.extend(set_font(
        BuiltinFont::HelveticaBold,
        TITLE_FONT_SIZE,
        black(),
    ));
    first_page_ops.extend(show_text_at(
        LEFT_MARGIN_MM,
        TITLE_Y_MM,
        "UnitPrep Audit Log Report".to_string(),
    ));

    first_page_ops.extend(set_font(BuiltinFont::Helvetica, META_FONT_SIZE, gray()));
    let mut meta_y = TITLE_Y_MM - 10.0;
    for line in [
        format!("Generated by: {}", report.generated_by),
        format!("Generated at: {}", report.generated_at),
        // Named once here rather than repeated on every row -- the Time
        // column's per-row format (see AuditLogPdfRow's caller) is
        // compact specifically because the timezone doesn't need to
        // repeat 5000 times to stay unambiguous.
        "All times shown in UTC.".to_string(),
    ] {
        first_page_ops.extend(show_text_at(LEFT_MARGIN_MM, meta_y, line));
        meta_y -= 6.0;
    }
    for line in &report.filter_lines {
        first_page_ops.extend(show_text_at(LEFT_MARGIN_MM, meta_y, line.clone()));
        meta_y -= 5.0;
    }

    let first_table_y = meta_y - 6.0;
    first_page_ops.extend(header_row_ops(first_table_y));

    let first_page_budget_mm = first_table_y - ROW_HEIGHT_MM - BOTTOM_MARGIN_MM;
    let later_page_budget_mm = TOP_START_MM - ROW_HEIGHT_MM - BOTTOM_MARGIN_MM;
    let mut row_pages =
        paginate_rows(&report.rows, first_page_budget_mm, later_page_budget_mm).into_iter();

    let first_chunk = row_pages.next().unwrap_or(&[]);
    let mut y = first_table_y - ROW_HEIGHT_MM;
    first_page_ops.extend(set_font(BuiltinFont::Helvetica, BODY_FONT_SIZE, black()));
    for row in first_chunk {
        first_page_ops.extend(data_row_ops(row, y));
        y -= row_height_mm(row);
    }
    first_page_ops.push(Op::EndTextSection);
    first_page_ops.push(Op::RestoreGraphicsState);
    pages.push(PdfPage::new(
        Mm(PAGE_WIDTH_MM),
        Mm(PAGE_HEIGHT_MM),
        first_page_ops,
    ));

    // Every later page starts the table right below the top margin --
    // no title block to make room for.
    for chunk in row_pages {
        let mut ops = vec![Op::SaveGraphicsState, Op::StartTextSection];
        ops.extend(header_row_ops(TOP_START_MM));
        ops.extend(set_font(BuiltinFont::Helvetica, BODY_FONT_SIZE, black()));
        let mut y = TOP_START_MM - ROW_HEIGHT_MM;
        for row in chunk {
            ops.extend(data_row_ops(row, y));
            y -= row_height_mm(row);
        }
        ops.push(Op::EndTextSection);
        ops.push(Op::RestoreGraphicsState);
        pages.push(PdfPage::new(Mm(PAGE_WIDTH_MM), Mm(PAGE_HEIGHT_MM), ops));
    }

    if let Some(shown) = report.truncated_at {
        let mut ops = vec![Op::SaveGraphicsState, Op::StartTextSection];
        ops.extend(set_font(BuiltinFont::HelveticaBold, BODY_FONT_SIZE, gray()));
        ops.extend(show_text_at(
            LEFT_MARGIN_MM,
            TOP_START_MM,
            format!(
                "Results truncated: showing the first {shown} matching events. \
                 Narrow your filters to see the rest."
            ),
        ));
        ops.push(Op::EndTextSection);
        ops.push(Op::RestoreGraphicsState);
        pages.push(PdfPage::new(Mm(PAGE_WIDTH_MM), Mm(PAGE_HEIGHT_MM), ops));
    }

    doc.with_pages(pages)
        .save(&PdfSaveOptions::default(), &mut Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard for the exact bug that shipped once: `Op::SetTextCursor`
    /// compiles to PDF's relative `Td` operator (see `show_text_at`'s own doc
    /// comment), so calling it repeatedly within one text section made every
    /// cell after the first drift further off-page instead of landing at the
    /// position each call actually asked for -- the PDF rendered as a title
    /// and one lone header cell, nothing else. Asserts the fix directly: each
    /// call produces its own absolute `SetTextMatrix`, unaffected by any call
    /// before it.
    #[test]
    fn show_text_at_produces_independent_absolute_positions() {
        let first = show_text_at(15.0, 232.0, "Time".to_string());
        let second = show_text_at(60.0, 232.0, "Actor".to_string());

        let Op::SetTextMatrix {
            matrix: TextMatrix::Translate(x, y),
        } = &first[0]
        else {
            panic!("expected an absolute SetTextMatrix for the first call");
        };
        assert!((x.0 - mm_to_pt(15.0)).abs() < 0.01);
        assert!((y.0 - mm_to_pt(232.0)).abs() < 0.01);

        let Op::SetTextMatrix {
            matrix: TextMatrix::Translate(x, y),
        } = &second[0]
        else {
            panic!("expected an absolute SetTextMatrix for the second call");
        };
        // The regression: under the old SetTextCursor/Td behaviour this
        // would read 15.0 + 60.0 = 75.0mm worth of pt, not a clean 60.0mm --
        // the second call's position would carry the first call's position
        // baked into it.
        assert!((x.0 - mm_to_pt(60.0)).abs() < 0.01);
        assert!((y.0 - mm_to_pt(232.0)).abs() < 0.01);
    }

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

    #[test]
    fn renders_a_small_report_to_nonempty_pdf_bytes() {
        let report = AuditLogPdfReport {
            generated_by: "Admin Admin (admin@example.com)".to_string(),
            generated_at: "2026-08-05T21:00:00Z".to_string(),
            filter_lines: vec![
                "Date range: 2026-08-01 to 2026-08-05".to_string(),
                "Events: All events".to_string(),
                "User: All users".to_string(),
            ],
            rows: (1..=3).map(sample_row).collect(),
            truncated_at: None,
        };

        let bytes = render_audit_log_pdf(&report);

        assert!(!bytes.is_empty());
        assert_eq!(&bytes[0..4], b"%PDF");
    }

    #[test]
    fn renders_a_truncation_note_when_capped() {
        let report = AuditLogPdfReport {
            generated_by: "Admin Admin (admin@example.com)".to_string(),
            generated_at: "2026-08-05T21:00:00Z".to_string(),
            filter_lines: vec![],
            rows: (1..=3).map(sample_row).collect(),
            truncated_at: Some(3),
        };

        let bytes = render_audit_log_pdf(&report);

        assert!(!bytes.is_empty());
        assert_eq!(&bytes[0..4], b"%PDF");
    }

    #[test]
    fn paginates_across_multiple_pages_for_a_large_row_count() {
        // Comfortably more rows than fit on one page at ROW_HEIGHT_MM
        // spacing -- forces the multi-page path to actually run.
        let report = AuditLogPdfReport {
            generated_by: "Admin Admin (admin@example.com)".to_string(),
            generated_at: "2026-08-05T21:00:00Z".to_string(),
            filter_lines: vec![],
            rows: (1..=200).map(sample_row).collect(),
            truncated_at: None,
        };

        let bytes = render_audit_log_pdf(&report);

        assert!(!bytes.is_empty());
        assert_eq!(&bytes[0..4], b"%PDF");
    }

    #[test]
    #[ignore = "manual visual inspection only -- writes to /tmp"]
    fn write_sample_pdf_to_disk_for_manual_inspection() {
        let report = AuditLogPdfReport {
            generated_by: "Boris Maksimov (bmaksimov@quikstor.com)".to_string(),
            generated_at: "2026-08-05T22:00:00Z".to_string(),
            filter_lines: vec![
                "Date range: 2026-08-01 to 2026-08-05".to_string(),
                "Events: All events".to_string(),
                "Users: All users".to_string(),
                "IP address: Any".to_string(),
            ],
            rows: (1..=60)
                .map(sample_row)
                .enumerate()
                .map(|(index, mut row)| {
                    // Row 4 (n=5, an otherwise-empty-Details login event)
                    // gets an artificially long Details value so this
                    // manual-inspection render actually exercises wrapping
                    // instead of only ever showing the short, single-line
                    // "status: active -> deactivated" the fixture
                    // otherwise alternates in.
                    if index == 4 {
                        row.details = "Session token rotated after step-up re-authentication; \
                             previous session revoked from IP 203.0.113.1 due to a role change \
                             from OnboardingManager to Admin"
                            .to_string();
                    }
                    row
                })
                .collect(),
            truncated_at: None,
        };

        let bytes = render_audit_log_pdf(&report);
        std::fs::write("/tmp/sample_audit_log.pdf", bytes).unwrap();
    }

    #[test]
    fn renders_with_zero_rows() {
        let report = AuditLogPdfReport {
            generated_by: "Admin Admin (admin@example.com)".to_string(),
            generated_at: "2026-08-05T21:00:00Z".to_string(),
            filter_lines: vec!["No matching events.".to_string()],
            rows: vec![],
            truncated_at: None,
        };

        let bytes = render_audit_log_pdf(&report);

        assert!(!bytes.is_empty());
        assert_eq!(&bytes[0..4], b"%PDF");
    }
}
