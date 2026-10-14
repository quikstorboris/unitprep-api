//! Reads Syrasoft's "Tenant List - All Tenants" report into an ordinary
//! table. Syrasoft's reports are printed layouts, not data exports, and
//! this one stacks a block of lines per tenant record:
//!
//! ```text
//! Kreger, Betty   [H] 305-304-8615   Balance: 0.0    Access #:
//!                 [C] 305-555-0100   Deposit: 0.0
//! 274 16th ST APT 7                  Email: ...
//! REEDSPORT, OR 97467
//!                 Unit # | Unit Type | Rate | Move In | Next Billing | Overlock
//! Comments:       0043   | 10 x 15   | ...
//! (comment text)  0017   | 10 x 20   | ...
//! ```
//!
//! There is no `Page N` marker (so `printed_report` does not apply) and
//! no tenant id anywhere in the report. The block is anchored on its
//! `Balance:` label; the unit lines sit in columns beside the comment
//! lines, under a header row repeated for every record.
//!
//! One output row per (tenant record, unit). Records with no unit line
//! are former tenants ("All Tenants" lists them all -- 6,000 records for
//! 530 current tenants at Longview) and are left out: they are not
//! tenants to migrate or to de-duplicate. Each row carries
//! `Tenant Record ID`, `R-` plus the record's first unit, so the several units of
//! one record stay together and two records under one name stay apart --
//! the same-person-entered-twice case dedup exists to find.
//!
//! Large facilities spill the report onto further sheets (Longview's has
//! two). The caller passes every sheet's rows concatenated; records never
//! straddle a sheet break.

const TITLE_PREFIX: &str = "tenant list";
const TITLE_SCAN_ROWS: usize = 6;
const MAX_PHONES: usize = 4;
const MAX_ADDRESS_LINES: usize = 3;

pub(super) struct FlattenedReport {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// True when `rows` (a sheet, row 0 first) is a Syrasoft Tenant List: its
/// title cell sits in the first few rows, next to the `Created:` stamp.
/// Cheap, so `excel.rs` can ask it before deciding to read other sheets.
pub(super) fn is_tenant_list(rows: &[Vec<String>]) -> bool {
    let has_title = rows.iter().take(TITLE_SCAN_ROWS).any(|row| {
        non_empty(row).any(|v| v.to_lowercase().starts_with(TITLE_PREFIX) && v.contains(" - "))
    });
    let has_created = rows
        .iter()
        .take(TITLE_SCAN_ROWS)
        .any(|row| non_empty(row).any(|v| v.starts_with("Created:")));
    has_title && has_created
}

fn non_empty(row: &[String]) -> impl Iterator<Item = &str> {
    row.iter().map(|v| v.trim()).filter(|v| !v.is_empty())
}

fn cell(row: &[String], idx: usize) -> &str {
    row.get(idx).map(|v| v.trim()).unwrap_or("")
}

fn position_of(row: &[String], label: &str) -> Option<usize> {
    row.iter().position(|v| v.trim() == label)
}

/// The first non-blank cell strictly right of `idx`.
fn first_after(row: &[String], idx: usize) -> String {
    row.iter()
        .skip(idx + 1)
        .map(|v| v.trim())
        .find(|v| !v.is_empty())
        .unwrap_or("")
        .to_string()
}

/// `[H]`, `[C]`, `[B]`: Syrasoft's phone-type tag.
fn is_phone_tag(value: &str) -> bool {
    let v = value.trim();
    v.len() == 3 && v.starts_with('[') && v.ends_with(']')
}

#[derive(Default)]
struct UnitLine {
    unit: String,
    unit_type: String,
    rate: String,
    move_in: String,
    next_billing: String,
    overlock: String,
}

#[derive(Default)]
struct Record {
    name: String,
    name_col: usize,
    phones: Vec<(String, String)>,
    address: Vec<String>,
    email: String,
    access: String,
    balance: String,
    deposit: String,
    comments: Vec<String>,
    units: Vec<UnitLine>,
    /// Left edges of the unit table's columns, taken from this record's
    /// own `Unit #` header row (None until that row is seen).
    unit_columns: Option<Vec<(usize, UnitField)>>,
}

#[derive(Clone, Copy)]
enum UnitField {
    Unit,
    Type,
    Rate,
    MoveIn,
    NextBilling,
    Overlock,
}

impl Record {
    fn take_phones(&mut self, row: &[String]) {
        for (i, v) in row.iter().enumerate() {
            if is_phone_tag(v) && self.phones.len() < MAX_PHONES {
                self.phones
                    .push((v.trim().to_string(), first_after(row, i)));
            }
        }
    }
}

/// Which unit-table field a data cell at `col` belongs to: the nearest
/// header edge at or left of it, so a value a column off its header
/// still lands in the right field.
fn unit_field_at(columns: &[(usize, UnitField)], col: usize) -> Option<UnitField> {
    let slot = columns.partition_point(|(start, _)| *start <= col);
    slot.checked_sub(1).map(|i| columns[i].1)
}

fn unit_columns_from_header(row: &[String]) -> Vec<(usize, UnitField)> {
    let mut columns: Vec<(usize, UnitField)> = row
        .iter()
        .enumerate()
        .filter_map(|(i, v)| {
            let field = match v.trim().to_lowercase().as_str() {
                "unit #" => UnitField::Unit,
                "unit type" => UnitField::Type,
                "rate" => UnitField::Rate,
                "move in" => UnitField::MoveIn,
                "next billing" => UnitField::NextBilling,
                "overlock" => UnitField::Overlock,
                _ => return None,
            };
            Some((i, field))
        })
        .collect();
    columns.sort_by_key(|(i, _)| *i);
    columns
}

/// `rows` is the whole report as text (every sheet, one after another).
/// Returns `None` when it is not a Syrasoft Tenant List or holds no
/// tenant with a unit.
pub(super) fn flatten_tenant_list(rows: &[Vec<String>]) -> Option<FlattenedReport> {
    if !is_tenant_list(rows) {
        return None;
    }

    let mut records: Vec<Record> = Vec::new();

    for row in rows {
        if non_empty(row).next().is_none() {
            continue;
        }

        // A record starts at its `Balance:` label row.
        if let Some(balance_at) = position_of(row, "Balance:") {
            let mut record = Record {
                balance: first_after(row, balance_at),
                ..Record::default()
            };
            let name_at = row.iter().position(|v| !v.trim().is_empty());
            if let Some(at) = name_at.filter(|&at| !is_phone_tag(&row[at]) && at < balance_at) {
                record.name = row[at].trim().to_string();
                record.name_col = at;
            }
            record.take_phones(row);
            if let Some(access_at) = position_of(row, "Access #:") {
                record.access = first_after(row, access_at);
            }
            records.push(record);
            continue;
        }

        let Some(record) = records.last_mut() else {
            continue;
        };

        if position_of(row, "Unit #").is_some() {
            record.unit_columns = Some(unit_columns_from_header(row));
            continue;
        }

        match record.unit_columns.clone() {
            None => {
                // The contact half of the block.
                record.take_phones(row);
                if let Some(at) = position_of(row, "Deposit:") {
                    record.deposit = first_after(row, at);
                }
                if let Some(at) = position_of(row, "Email:") {
                    record.email = first_after(row, at);
                }
                let text = cell(row, record.name_col);
                if !text.is_empty() && record.address.len() < MAX_ADDRESS_LINES {
                    record.address.push(text.to_string());
                }
            }
            Some(columns) => {
                // The unit half: unit lines in the table columns, the
                // record's comment text in the name column beside them.
                let text = cell(row, record.name_col);
                if !text.is_empty() && text != "Comments:" {
                    record.comments.push(text.to_string());
                }

                let table_start = columns.first().map(|(c, _)| *c).unwrap_or(usize::MAX);
                let mut line = UnitLine::default();
                for (col, value) in row.iter().enumerate().skip(table_start) {
                    let value = value.trim();
                    if value.is_empty() {
                        continue;
                    }
                    let target = match unit_field_at(&columns, col) {
                        Some(UnitField::Unit) => &mut line.unit,
                        Some(UnitField::Type) => &mut line.unit_type,
                        Some(UnitField::Rate) => &mut line.rate,
                        Some(UnitField::MoveIn) => &mut line.move_in,
                        Some(UnitField::NextBilling) => &mut line.next_billing,
                        Some(UnitField::Overlock) => &mut line.overlock,
                        None => continue,
                    };
                    if !target.is_empty() {
                        target.push(' ');
                    }
                    target.push_str(value);
                }
                if !line.unit.is_empty() {
                    record.units.push(line);
                }
            }
        }
    }

    let mut headers: Vec<String> = vec!["Tenant".to_string(), "Tenant Record ID".to_string()];
    for n in 1..=MAX_PHONES {
        headers.push(format!("Phone Type {n}"));
        headers.push(format!("Phone {n}"));
    }
    for n in 1..=MAX_ADDRESS_LINES {
        headers.push(format!("Address Line {n}"));
    }
    headers.extend(
        [
            "Email",
            "Access #",
            "Balance",
            "Deposit",
            "Comments",
            "Unit #",
            "Unit Type",
            "Rate",
            "Move In",
            "Next Billing",
            "Overlock",
        ]
        .map(String::from),
    );

    let mut data: Vec<Vec<String>> = Vec::new();
    for record in &records {
        let Some(first) = record.units.first() else {
            continue;
        };
        for unit in &record.units {
            let mut row = vec![record.name.clone(), format!("R-{}", first.unit)];
            for n in 0..MAX_PHONES {
                let (tag, number) = record.phones.get(n).cloned().unwrap_or_default();
                row.push(tag);
                row.push(number);
            }
            for n in 0..MAX_ADDRESS_LINES {
                row.push(record.address.get(n).cloned().unwrap_or_default());
            }
            row.push(record.email.clone());
            row.push(record.access.clone());
            row.push(record.balance.clone());
            row.push(record.deposit.clone());
            row.push(record.comments.join(" "));
            row.push(unit.unit.clone());
            row.push(unit.unit_type.clone());
            row.push(unit.rate.clone());
            row.push(unit.move_in.clone());
            row.push(unit.next_billing.clone());
            row.push(unit.overlock.clone());
            data.push(row);
        }
    }

    if data.is_empty() {
        return None;
    }

    Some(FlattenedReport {
        headers,
        rows: data,
    })
}

#[cfg(test)]
#[path = "syrasoft_report_tests.rs"]
mod tests;
