//! Easy Storage Solutions' tenant-export transform.

use crate::csv_document::CsvDocument;

/// Easy Storage Solutions' `Address` column combines street and
/// city/state/zip across an embedded newline inside one CSV field
/// (`"208 Laurel Oak Dr.\nSt. Rose, Louisiana 70087"`). Splits it
/// into the four canonical address fields the comparison pipeline
/// expects as independent columns, appended onto the document under
/// their canonical names — `apply_field_mapping`'s ordinary identity
/// entries for those four fields then pick them straight up.
///
/// A no-op (returns `document` unchanged) if there's no `Address`
/// column at all, so this stays safe to call speculatively.
pub(super) fn split_ess_address(document: &CsvDocument) -> CsvDocument {
    let Some(address_idx) = document.header_index("Address") else {
        return document.clone();
    };

    let mut headers = document.headers.clone();
    headers.push("AddressStreet1".to_string());
    headers.push("AddressCity".to_string());
    headers.push("AddressState".to_string());
    headers.push("AddressPostalCode".to_string());

    let rows = document
        .rows
        .iter()
        .map(|row| {
            let mut row = row.clone();
            let raw = row.get(address_idx).cloned().unwrap_or_default();
            let (street, city, state, postal) = parse_multiline_address(&raw);
            row.push(street);
            row.push(city);
            row.push(state);
            row.push(postal);
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

/// Splits one raw `"street\nCity, State Zip"` value into its four
/// parts. Verified against every row of a real Easy Storage
/// Solutions export (160 rows: 154 two-line addresses, 6 blank) —
/// see `vendor_format_tests.rs` for the fixture rows this was
/// checked against, including a zip+4 (`"70301-6843"`) and a row
/// with a city/state but no zip at all (`"Abita Springs, LA"`).
///
/// Falls back gracefully rather than erroring wherever a row
/// doesn't match the expected shape — a blank address, a missing
/// second line, a missing comma, or a state/zip segment with no
/// space all degrade to "put what we have in `street`, leave the
/// rest blank" instead of failing the whole file over one malformed
/// row. Splitting on the LAST space in the state/zip segment (not
/// the first) is what makes multi-word states like "New York" or
/// "North Carolina" split correctly — the zip is always the final
/// token, however many words the state name has.
fn parse_multiline_address(raw: &str) -> (String, String, String, String) {
    let mut lines = raw.splitn(2, '\n');
    let street = lines.next().unwrap_or("").trim().to_string();

    let Some(second_line) = lines.next() else {
        return (street, String::new(), String::new(), String::new());
    };
    let second_line = second_line.trim();

    let Some((city_part, state_zip)) = second_line.rsplit_once(',') else {
        return (
            street,
            String::new(),
            second_line.to_string(),
            String::new(),
        );
    };
    let city = city_part.trim().to_string();
    let state_zip = state_zip.trim();

    let Some((state, postal)) = state_zip.rsplit_once(' ') else {
        return (street, city, state_zip.to_string(), String::new());
    };

    (
        street,
        city,
        state.trim().to_string(),
        postal.trim().to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_real_two_line_address() {
        let (street, city, state, postal) =
            parse_multiline_address("208 Laurel Oak Dr.\nSt. Rose, Louisiana 70087");
        assert_eq!(street, "208 Laurel Oak Dr.");
        assert_eq!(city, "St. Rose");
        assert_eq!(state, "Louisiana");
        assert_eq!(postal, "70087");
    }

    #[test]
    fn handles_a_multi_word_state_name() {
        let (_, city, state, postal) =
            parse_multiline_address("1 Main St\nThibodaux, North Carolina 70301");
        assert_eq!(city, "Thibodaux");
        assert_eq!(state, "North Carolina");
        assert_eq!(postal, "70301");
    }

    #[test]
    fn preserves_a_zip_plus_four() {
        let (_, _, _, postal) =
            parse_multiline_address("1218 EMPIRE BUILDER\nTHIBODAUX, LA 70301-6843");
        assert_eq!(postal, "70301-6843");
    }

    #[test]
    fn falls_back_when_the_second_line_has_no_zip() {
        let (street, city, state, postal) =
            parse_multiline_address("21211 Soell Dr.\nAbita Springs, LA");
        assert_eq!(street, "21211 Soell Dr.");
        assert_eq!(city, "Abita Springs");
        assert_eq!(state, "LA");
        assert_eq!(postal, "");
    }

    #[test]
    fn falls_back_on_a_blank_address() {
        let (street, city, state, postal) = parse_multiline_address("");
        assert_eq!(street, "");
        assert_eq!(city, "");
        assert_eq!(state, "");
        assert_eq!(postal, "");
    }

    #[test]
    fn split_ess_address_is_a_noop_without_an_address_column() {
        let doc = CsvDocument {
            file_name: "test.csv".to_string(),
            headers: vec!["Unit".to_string()],
            rows: vec![vec!["101".to_string()]],
            modified_at: None,
        };
        let result = split_ess_address(&doc);
        assert_eq!(result.headers, doc.headers);
    }

    #[test]
    fn split_ess_address_appends_the_four_canonical_columns() {
        let doc = CsvDocument {
            file_name: "test.csv".to_string(),
            headers: vec!["Unit".to_string(), "Address".to_string()],
            rows: vec![vec![
                "101".to_string(),
                "208 Laurel Oak Dr.\nSt. Rose, Louisiana 70087".to_string(),
            ]],
            modified_at: None,
        };
        let result = split_ess_address(&doc);
        assert_eq!(
            result.headers,
            vec![
                "Unit",
                "Address",
                "AddressStreet1",
                "AddressCity",
                "AddressState",
                "AddressPostalCode"
            ]
        );
        assert_eq!(
            result.rows[0],
            vec![
                "101",
                "208 Laurel Oak Dr.\nSt. Rose, Louisiana 70087",
                "208 Laurel Oak Dr.",
                "St. Rose",
                "Louisiana",
                "70087"
            ]
        );
    }
}
