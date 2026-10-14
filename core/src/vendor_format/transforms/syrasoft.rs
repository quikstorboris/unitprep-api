//! Syrasoft's tenant-export transform (the "Tenant List - All Tenants"
//! report, already flattened to one row per tenant record and unit by
//! `parsing::syrasoft_report`).

use super::{cell, collapse_whitespace};
use crate::csv_document::CsvDocument;

/// Syrasoft's phone-type tags in the order dedup prefers them when a
/// record lists several: home, cell, business. A record whose only
/// phones are other tags falls back to its first listed phone.
const PHONE_PREFERENCE: &[&str] = &["[H]", "[C]", "[B]"];

/// Syrasoft's tenant list has no single name field in dedup's shape, no
/// single phone, and an address as free lines, so this derives the
/// canonical columns (appended under their canonical names, picked up by
/// `apply_field_mapping`'s ordinary identity entries):
///
/// - `FirtLast`: the report writes names "Last, First [Middle]", so this
///   is the first given name then the last name ("Kreger, Betty Jo" ->
///   "Betty Kreger"; middle names left out so a record that differs only
///   by one still groups). A name with no comma (a business) is kept as
///   written, whitespace collapsed.
/// - `PhoneNumber`: see `PHONE_PREFERENCE`, with the note text staff type
///   after the number ("360-577-1640 sisters") cut off.
/// - `AddressStreet1/2/City/State/PostalCode`: the last address line is
///   "City, ST 12345"; the lines before it are the street.
///
/// Also blanks `Email` where it holds something that is not an address:
/// the report has staff notes ("no listed alternate") in that slot.
/// Every source column is optional; one that's absent contributes a
/// blank.
pub(super) fn derive_syrasoft_tenant_fields(document: &CsvDocument) -> CsvDocument {
    let email_idx = document.header_index("Email");

    let mut headers = document.headers.clone();
    for name in [
        "FirtLast",
        "PhoneNumber",
        "AddressStreet1",
        "AddressStreet2",
        "AddressCity",
        "AddressState",
        "AddressPostalCode",
    ] {
        headers.push(name.to_string());
    }

    let rows = document
        .rows
        .iter()
        .map(|row| {
            let mut row = row.clone();

            let first_last = first_last_key(&cell(document, &row, "Tenant"));
            let phone = preferred_phone(document, &row);
            let address = split_address(
                [
                    cell(document, &row, "Address Line 1"),
                    cell(document, &row, "Address Line 2"),
                    cell(document, &row, "Address Line 3"),
                ]
                .into_iter()
                .filter(|line| !line.is_empty())
                .collect(),
            );

            if let Some(idx) = email_idx {
                if row.get(idx).is_some_and(|v| !v.contains('@')) {
                    row[idx].clear();
                }
            }

            row.push(first_last);
            row.push(phone);
            row.push(address.street1);
            row.push(address.street2);
            row.push(address.city);
            row.push(address.state);
            row.push(address.postal_code);
            row
        })
        .collect();

    CsvDocument {
        file_name: document.file_name.clone(),
        headers,
        rows,
        modified_at: document.modified_at,
    }
}

/// "Last, First [Middle]" -> "First Last"; anything without a comma is
/// returned as written (whitespace collapsed).
fn first_last_key(name: &str) -> String {
    let name = collapse_whitespace(name);
    match name.split_once(',') {
        Some((last, given)) => {
            let first = given.split_whitespace().next().unwrap_or("");
            collapse_whitespace(&format!("{first} {}", last.trim()))
        }
        None => name,
    }
}

fn preferred_phone(document: &CsvDocument, row: &[String]) -> String {
    let phones: Vec<(String, String)> = (1..=4)
        .map(|n| {
            (
                cell(document, row, &format!("Phone Type {n}")),
                cell(document, row, &format!("Phone {n}")),
            )
        })
        .filter(|(_, number)| number.chars().any(|c| c.is_ascii_digit()))
        .collect();

    PHONE_PREFERENCE
        .iter()
        .find_map(|tag| phones.iter().find(|(t, _)| t == tag))
        .or_else(|| phones.first())
        .map(|(_, number)| strip_phone_note(number))
        .unwrap_or_default()
}

/// The number itself, without the words staff typed after it: from the
/// first digit (or `+`/`(`) through the last character that can be part
/// of a phone number.
fn strip_phone_note(value: &str) -> String {
    let start = value
        .find(|c: char| c.is_ascii_digit() || c == '+' || c == '(')
        .unwrap_or(0);
    let tail = &value[start..];
    let end = tail.find(|c: char| c.is_alphabetic()).unwrap_or(tail.len());
    tail[..end]
        .trim_end_matches(|c: char| !c.is_ascii_digit() && c != ')')
        .trim()
        .to_string()
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Address {
    street1: String,
    street2: String,
    city: String,
    state: String,
    postal_code: String,
}

/// `lines` are the non-blank address lines in order. The last line is
/// the city line when it reads "City, ST 12345"; a record with a city
/// line and no street has just that one line. A last line that does not
/// parse is kept whole as the city rather than guessed at.
fn split_address(mut lines: Vec<String>) -> Address {
    let mut address = Address::default();
    let Some(last) = lines.pop() else {
        return address;
    };

    match parse_city_line(&last) {
        Some((city, state, postal_code)) => {
            address.city = city;
            address.state = state;
            address.postal_code = postal_code;
        }
        None if lines.is_empty() => {
            // A single line that is not a city line is the street.
            address.street1 = last;
            return address;
        }
        None => address.city = last,
    }

    let mut street = lines.into_iter();
    address.street1 = street.next().unwrap_or_default();
    address.street2 = street.collect::<Vec<_>>().join(" ");
    address
}

/// "Reedsport, OR 97467" or "Reedsport, OR 97467-1234" -> (city, state,
/// zip). Strict on the state (two letters) and zip (5 or 5+4 digits) so
/// a street line with a comma ("12 Main St, Apt 4") is never read as one.
fn parse_city_line(line: &str) -> Option<(String, String, String)> {
    let (city, rest) = line.rsplit_once(',')?;
    let mut parts = rest.split_whitespace();
    let state = parts.next()?;
    let zip = parts.next()?;
    if parts.next().is_some() {
        return None;
    }

    let state_ok = state.len() == 2 && state.chars().all(|c| c.is_ascii_alphabetic());
    let zip_ok = match zip.split_once('-') {
        Some((a, b)) => {
            a.len() == 5 && b.len() == 4 && a.chars().chain(b.chars()).all(|c| c.is_ascii_digit())
        }
        None => zip.len() == 5 && zip.chars().all(|c| c.is_ascii_digit()),
    };
    let city = collapse_whitespace(city);
    (state_ok && zip_ok && !city.is_empty()).then(|| (city, state.to_string(), zip.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADERS: &[&str] = &[
        "Tenant",
        "Tenant Record ID",
        "Phone Type 1",
        "Phone 1",
        "Phone Type 2",
        "Phone 2",
        "Phone Type 3",
        "Phone 3",
        "Phone Type 4",
        "Phone 4",
        "Address Line 1",
        "Address Line 2",
        "Address Line 3",
        "Email",
        "Unit #",
    ];

    fn doc(rows: Vec<Vec<String>>) -> CsvDocument {
        CsvDocument {
            file_name: "Tenant List - All Tenants.xls".to_string(),
            headers: HEADERS.iter().map(|h| h.to_string()).collect(),
            rows,
            modified_at: None,
        }
    }

    /// name, phones [(tag, number)], address lines, email.
    fn row(name: &str, phones: &[(&str, &str)], address: &[&str], email: &str) -> Vec<String> {
        let mut cells: Vec<String> = vec![name.to_string(), "0001".to_string()];
        for n in 0..4 {
            let (tag, number) = phones.get(n).copied().unwrap_or(("", ""));
            cells.push(tag.to_string());
            cells.push(number.to_string());
        }
        for n in 0..3 {
            cells.push(address.get(n).copied().unwrap_or("").to_string());
        }
        cells.push(email.to_string());
        cells.push("0001".to_string());
        cells
    }

    fn derived(out: &CsvDocument, header: &str) -> String {
        let idx = out.header_index(header).expect("derived column present");
        out.rows[0][idx].clone()
    }

    #[test]
    fn name_becomes_first_then_last_without_the_middle_name() {
        assert_eq!(first_last_key("Kreger, Betty"), "Betty Kreger");
        assert_eq!(first_last_key("Caldwell,  Tina   Marie"), "Tina Caldwell");
        assert_eq!(first_last_key("MARKS, BILL  AND MAUREEN"), "BILL MARKS");
        assert_eq!(first_last_key("Adams Jr, Paul E."), "Paul Adams Jr");
    }

    #[test]
    fn a_name_without_a_comma_is_kept_as_written() {
        assert_eq!(
            first_last_key("Addcox  Heating Center"),
            "Addcox Heating Center"
        );
    }

    #[test]
    fn phone_prefers_home_then_cell_then_business_and_drops_trailing_notes() {
        let d = doc(vec![row(
            "A, B",
            &[
                ("[B]", "111-111-1111"),
                ("[C]", "222-222-2222 text"),
                ("[H]", "(333) 333-3333 sisters"),
            ],
            &[],
            "",
        )]);
        assert_eq!(
            derived(&derive_syrasoft_tenant_fields(&d), "PhoneNumber"),
            "(333) 333-3333"
        );

        let d = doc(vec![row(
            "A, B",
            &[("[B]", "111-111-1111"), ("[C]", "222-222-2222 text")],
            &[],
            "",
        )]);
        assert_eq!(
            derived(&derive_syrasoft_tenant_fields(&d), "PhoneNumber"),
            "222-222-2222"
        );
    }

    #[test]
    fn phone_without_digits_is_ignored_and_none_gives_blank() {
        let d = doc(vec![row("A, B", &[("[H]", "none")], &[], "")]);
        assert_eq!(
            derived(&derive_syrasoft_tenant_fields(&d), "PhoneNumber"),
            ""
        );
        let d = doc(vec![row("A, B", &[], &[], "")]);
        assert_eq!(
            derived(&derive_syrasoft_tenant_fields(&d), "PhoneNumber"),
            ""
        );
    }

    #[test]
    fn phone_tag_outside_the_known_three_falls_back_to_first_listed() {
        let d = doc(vec![row("A, B", &[("[X]", "444-444-4444")], &[], "")]);
        assert_eq!(
            derived(&derive_syrasoft_tenant_fields(&d), "PhoneNumber"),
            "444-444-4444"
        );
    }

    #[test]
    fn address_splits_street_and_city_line() {
        let d = doc(vec![row(
            "A, B",
            &[],
            &["1519 20th Ave #3", "Longview, WA 98632"],
            "",
        )]);
        let out = derive_syrasoft_tenant_fields(&d);
        assert_eq!(derived(&out, "AddressStreet1"), "1519 20th Ave #3");
        assert_eq!(derived(&out, "AddressCity"), "Longview");
        assert_eq!(derived(&out, "AddressState"), "WA");
        assert_eq!(derived(&out, "AddressPostalCode"), "98632");
    }

    #[test]
    fn a_lone_city_line_is_a_city_and_a_lone_street_is_a_street() {
        let d = doc(vec![row("A, B", &[], &["Longview, WA 98632"], "")]);
        let out = derive_syrasoft_tenant_fields(&d);
        assert_eq!(derived(&out, "AddressStreet1"), "");
        assert_eq!(derived(&out, "AddressCity"), "Longview");

        let d = doc(vec![row("A, B", &[], &["PO Box 863"], "")]);
        let out = derive_syrasoft_tenant_fields(&d);
        assert_eq!(derived(&out, "AddressStreet1"), "PO Box 863");
        assert_eq!(derived(&out, "AddressCity"), "");
    }

    #[test]
    fn three_lines_make_a_second_street_line() {
        let d = doc(vec![row(
            "A, B",
            &[],
            &["3024 Maryland St.", "#12", "Longview, WA 98632-1234"],
            "",
        )]);
        let out = derive_syrasoft_tenant_fields(&d);
        assert_eq!(derived(&out, "AddressStreet2"), "#12");
        assert_eq!(derived(&out, "AddressPostalCode"), "98632-1234");
    }

    #[test]
    fn a_street_with_a_comma_is_not_read_as_a_city_line() {
        assert_eq!(parse_city_line("12 Main St, Apt 4"), None);
        assert_eq!(parse_city_line("Town, OR 9700"), None);
        let d = doc(vec![row(
            "A, B",
            &[],
            &["12 Main St, Apt 4", "Elsewhere"],
            "",
        )]);
        let out = derive_syrasoft_tenant_fields(&d);
        // Unparseable last line is kept whole as the city, never dropped.
        assert_eq!(derived(&out, "AddressCity"), "Elsewhere");
        assert_eq!(derived(&out, "AddressStreet1"), "12 Main St, Apt 4");
    }

    #[test]
    fn email_keeps_real_addresses_and_blanks_staff_notes() {
        let d = doc(vec![
            row("A, B", &[], &[], "real@x.com"),
            row("A, B", &[], &[], "no listed alternate"),
        ]);
        let out = derive_syrasoft_tenant_fields(&d);
        let idx = out.header_index("Email").unwrap();
        assert_eq!(out.rows[0][idx], "real@x.com");
        assert_eq!(out.rows[1][idx], "");
    }

    #[test]
    fn tolerates_missing_source_columns() {
        let d = CsvDocument {
            file_name: "x.xls".to_string(),
            headers: vec!["Tenant".to_string()],
            rows: vec![vec!["Solo, Sam".to_string()]],
            modified_at: None,
        };
        let out = derive_syrasoft_tenant_fields(&d);
        assert_eq!(derived(&out, "FirtLast"), "Sam Solo");
        assert_eq!(derived(&out, "PhoneNumber"), "");
        assert_eq!(derived(&out, "AddressCity"), "");
    }
}
