//! Reads a PMS "printed report" saved as a spreadsheet into an ordinary
//! table. Winsen's reports (and others like them) are page layouts, not
//! data exports: a title row, a "Page 1" cell, a facility/date/time row,
//! a one- to three-line header whose labels sit in sparse columns, blank
//! rows between records, and the header repeated at the top of every page.
//! The generic reader assumes row 0 is the header, so these files need
//! this pass first.
//!
//! The pass only fires when the sheet clearly is a printed report (a
//! `Page N` cell in the first rows and a sparse title row), so an ordinary
//! export is never touched.
//!
//! Alignment: a data cell belongs to the header whose left edge is the
//! nearest one at or to the left of it. Reports left-align their header
//! text but their data can start a column or two to the right of it (the
//! email report puts the address one column right of its header), so
//! matching by identical column would silently drop those cells.

pub(super) struct FlattenedReport {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

const MAX_HEADER_LINES: usize = 3;
const MAX_TITLE_ROWS: usize = 12;

/// `rows` is the whole sheet as text, row 0 first. Returns `None` when
/// this is not a printed report (the caller then reads it normally).
pub(super) fn flatten(rows: &[Vec<String>]) -> Option<FlattenedReport> {
    if !looks_like_printed_report(rows) {
        return None;
    }

    let header_start = (1..rows.len().min(MAX_TITLE_ROWS + MAX_HEADER_LINES))
        .find(|&i| is_header_candidate(&rows[i]))?;

    let mut header_end = header_start + 1;
    while header_end < rows.len()
        && header_end - header_start < MAX_HEADER_LINES
        && is_header_continuation(&rows[header_end])
    {
        header_end += 1;
    }

    let block = &rows[header_start..header_end];

    // Left edge of each header column: every column holding a label in any
    // header line, in order.
    let mut starts: Vec<usize> = block
        .iter()
        .flat_map(|line| {
            line.iter()
                .enumerate()
                .filter(|(_, v)| !v.trim().is_empty())
                .map(|(c, _)| c)
        })
        .collect();
    starts.sort_unstable();
    starts.dedup();

    if starts.len() < 2 {
        return None;
    }

    let headers: Vec<String> = starts
        .iter()
        .map(|&start| {
            block
                .iter()
                .filter_map(|line| line.get(start))
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();

    let label_tokens: Vec<String> = block
        .iter()
        .flatten()
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty())
        .collect();

    let mut data: Vec<Vec<String>> = Vec::new();

    for row in &rows[header_end..] {
        if !is_data_row(row, &label_tokens) {
            continue;
        }

        let mut record = vec![String::new(); starts.len()];

        for (col, value) in row.iter().enumerate() {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }

            // Nearest header edge at or left of this cell; a cell left of
            // every edge belongs to the first column.
            let slot = starts.partition_point(|&s| s <= col).saturating_sub(1);

            if record[slot].is_empty() {
                record[slot] = value.to_string();
            } else {
                record[slot].push(' ');
                record[slot].push_str(value);
            }
        }

        // Every record row has a value under the first header (the unit);
        // a totals line puts its figures further right.
        if record[0].is_empty() {
            continue;
        }

        data.push(record);
    }

    if data.is_empty() {
        return None;
    }

    Some(FlattenedReport {
        headers,
        rows: data,
    })
}

fn non_empty(row: &[String]) -> impl Iterator<Item = &str> {
    row.iter().map(|v| v.trim()).filter(|v| !v.is_empty())
}

fn looks_like_printed_report(rows: &[Vec<String>]) -> bool {
    let has_page_marker = rows.iter().take(3).any(|row| {
        non_empty(row).any(|v| {
            let lower = v.to_lowercase();
            lower
                .strip_prefix("page ")
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        })
    });

    let sparse_title = rows.first().is_some_and(|row| non_empty(row).count() <= 3);

    has_page_marker && sparse_title
}

fn is_numeric(value: &str) -> bool {
    value.parse::<f64>().is_ok()
}

fn is_time_or_date(value: &str) -> bool {
    let v = value.to_lowercase();
    let time = v.contains(':')
        && v.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ':' | ' ' | 'a' | 'p' | 'm'));
    let date = v.contains('/') && v.chars().all(|c| c.is_ascii_digit() || c == '/');
    time || date
}

/// A row of at least two text labels, none of them a number, a date or a
/// time (which is what the facility/date/time row above the header holds).
fn is_header_candidate(row: &[String]) -> bool {
    let cells: Vec<&str> = non_empty(row).collect();
    cells.len() >= 2 && cells.iter().all(|v| !is_numeric(v) && !is_time_or_date(v))
}

fn is_header_continuation(row: &[String]) -> bool {
    let cells: Vec<&str> = non_empty(row).collect();
    !cells.is_empty() && cells.iter().all(|v| !is_numeric(v) && !is_time_or_date(v))
}

fn is_data_row(row: &[String], label_tokens: &[String]) -> bool {
    let mut cells = non_empty(row);
    let Some(first) = cells.next() else {
        return false;
    };

    if cells.next().is_none() {
        return false;
    }

    let lower = first.to_lowercase();

    !label_tokens.contains(&lower)
        && !first.contains(char::is_whitespace)
        && first.chars().count() <= 12
        && !lower.starts_with("total")
        && !first.ends_with(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(cells: &[(usize, &str)], width: usize) -> Vec<String> {
        let mut row = vec![String::new(); width];
        for (c, v) in cells {
            row[*c] = (*v).to_string();
        }
        row
    }

    fn header_line() -> Vec<String> {
        r(
            &[
                (0, "Unit"),
                (2, "Customer Name"),
                (4, "Address Line 1"),
                (8, "City"),
                (12, "Res. Phone"),
            ],
            15,
        )
    }

    fn sheet() -> Vec<Vec<String>> {
        vec![
            r(&[(0, "Tenant Cross Reference"), (14, "Page 1")], 15),
            r(&[], 15),
            r(
                &[(0, "Some Facility"), (11, "46254"), (13, "3:11:29 PM")],
                15,
            ),
            r(&[], 15),
            header_line(),
            r(&[], 15),
            r(&[], 15),
            r(
                &[
                    (0, "00001"),
                    (2, "Ann Lee"),
                    (4, "1 Main St"),
                    (8, "Town"),
                    (12, "815-555-0100"),
                ],
                15,
            ),
            r(&[], 15),
            r(
                &[
                    (0, "00002"),
                    (2, "Bo Ray"),
                    (4, "2 Oak Rd"),
                    (8, "Town"),
                    (12, "815-555-0101"),
                ],
                15,
            ),
            r(&[(0, "Page 2")], 15),
            header_line(),
            r(&[], 15),
            r(
                &[
                    (0, "00003"),
                    (2, "Cy Poe"),
                    (4, "3 Elm Ln"),
                    (8, "Town"),
                    (12, "815-555-0102"),
                ],
                15,
            ),
            r(&[(0, "Total units:"), (2, "3")], 15),
        ]
    }

    #[test]
    fn flattens_a_printed_report_dropping_title_page_and_repeated_header_rows() {
        let out = flatten(&sheet()).expect("is a printed report");
        assert_eq!(
            out.headers,
            [
                "Unit",
                "Customer Name",
                "Address Line 1",
                "City",
                "Res. Phone"
            ]
        );
        let units: Vec<&str> = out.rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(units, ["00001", "00002", "00003"]);
        assert_eq!(out.rows[0][1], "Ann Lee");
        assert_eq!(out.rows[2][4], "815-555-0102");
    }

    #[test]
    fn a_data_cell_one_column_right_of_its_header_still_lands_under_it() {
        let email_header = || {
            r(
                &[
                    (0, "Unit"),
                    (3, "Customer Name"),
                    (6, "Customer Email Address"),
                ],
                15,
            )
        };
        let rows = vec![
            r(&[(0, "Tenant Email Address Report"), (13, "Page 1")], 15),
            r(&[], 15),
            r(
                &[(0, "Some Facility"), (11, "46254"), (12, "3:07:27 PM")],
                15,
            ),
            r(&[], 15),
            email_header(),
            r(&[], 15),
            r(&[(0, "00001"), (3, "Ann Lee"), (7, "ann@example.com")], 15),
            r(&[(0, "00002"), (3, "Bo Ray"), (7, "bo@example.com")], 15),
            r(&[(0, "Page 2")], 15),
            email_header(),
            r(&[(0, "00003"), (3, "Cy Poe"), (7, "cy@example.com")], 15),
        ];
        let out = flatten(&rows).unwrap();
        assert_eq!(
            out.headers,
            ["Unit", "Customer Name", "Customer Email Address"]
        );
        assert_eq!(out.rows.len(), 3);
        assert_eq!(out.rows[1][2], "bo@example.com");
    }

    #[test]
    fn a_two_line_header_is_joined_into_one_label() {
        let rows = vec![
            r(&[(0, "Rent Roll Report")], 12),
            r(&[(11, "Page 1")], 12),
            r(
                &[(0, "Some Facility"), (9, "46254"), (10, "3:05:03 PM")],
                12,
            ),
            r(&[], 12),
            r(&[(7, "Cust"), (9, "Beginning")], 12),
            r(
                &[(0, "Unit"), (2, "Customer Name"), (7, "ID"), (9, "Balance")],
                12,
            ),
            r(&[], 12),
            r(&[(0, "00001"), (2, "Ann Lee"), (7, "55"), (9, "0")], 12),
            r(&[(0, "00002"), (2, "Bo Ray"), (7, "181"), (9, "10")], 12),
        ];
        let out = flatten(&rows).unwrap();
        assert_eq!(
            out.headers,
            ["Unit", "Customer Name", "Cust ID", "Beginning Balance"]
        );
        assert_eq!(out.rows[0][2], "55");
        assert_eq!(out.rows[1][3], "10");
    }

    #[test]
    fn an_ordinary_export_is_left_alone() {
        let rows = vec![
            vec!["Unit".to_string(), "Name".to_string(), "Email".to_string()],
            vec!["1".to_string(), "Ann".to_string(), "a@x.com".to_string()],
        ];
        assert!(flatten(&rows).is_none());
    }

    #[test]
    fn a_report_with_no_data_rows_is_not_flattened() {
        let rows: Vec<Vec<String>> = sheet().into_iter().take(7).collect();
        assert!(flatten(&rows).is_none());
    }
}
