//! Joins a tenant file with the other reports of the same system that
//! each carry a few more fields for the same tenants. Winsen is the first
//! case: its contact report has the address and phone, a separate report
//! has the email, and a third has the customer id, none of them in the
//! others. A joined run reads the main (primary) file's rows and fills in
//! what the other (join) files know about the same tenant.
//!
//! Both sides are already in canonical column names (each file went
//! through its own registry mapping), so the join is generic: it matches
//! rows on `UnitNumber` + `FirtLast` and copies across every other
//! canonical column the join file supplies. Nothing here knows about any
//! one vendor. A value already present in the main file is never
//! overwritten; a join file only fills blanks and adds columns the main
//! file lacks.

use std::collections::HashMap;

use anyhow::{bail, Result};
use unitprep_core::csv_document::CsvDocument;

use crate::grouping::group_key;

const UNIT: &str = "UnitNumber";
const NAME: &str = "FirtLast";

fn row_key(unit: &str, name: &str) -> (String, String) {
    (unit.trim().to_lowercase(), group_key(name))
}

/// `primary` and each of `joins` are canonical-column documents. Returns
/// `primary` with the join files' extra fields attached. Errors when a
/// join file has no unit or no name column to match on, because rows from
/// it could then only be attached by guessing.
pub fn merge_joined(mut primary: CsvDocument, joins: &[CsvDocument]) -> Result<CsvDocument> {
    let (Some(unit_idx), Some(name_idx)) = (primary.header_index(UNIT), primary.header_index(NAME))
    else {
        bail!("The main file has no unit or customer name column, so the other files can't be matched to it");
    };

    for join in joins {
        let (Some(j_unit), Some(j_name)) = (join.header_index(UNIT), join.header_index(NAME))
        else {
            bail!(
                "'{}' has no unit or customer name column, so it can't be matched to the main file",
                join.file_name
            );
        };

        // First row wins when a key repeats.
        let mut by_key: HashMap<(String, String), &Vec<String>> = HashMap::new();
        for row in &join.rows {
            let (Some(unit), Some(name)) = (row.get(j_unit), row.get(j_name)) else {
                continue;
            };
            by_key.entry(row_key(unit, name)).or_insert(row);
        }

        // Every column the join supplies except the two it matched on.
        let extras: Vec<(usize, String)> = join
            .headers
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != j_unit && *i != j_name)
            .map(|(i, h)| (i, h.clone()))
            .collect();

        for (_, header) in &extras {
            if primary.header_index(header).is_none() {
                primary.headers.push(header.clone());
                for row in &mut primary.rows {
                    row.push(String::new());
                }
            }
        }

        let targets: Vec<(usize, usize)> = extras
            .iter()
            .filter_map(|(src, header)| primary.header_index(header).map(|dst| (*src, dst)))
            .collect();

        for row in &mut primary.rows {
            let (Some(unit), Some(name)) = (row.get(unit_idx), row.get(name_idx)) else {
                continue;
            };
            let Some(source) = by_key.get(&row_key(unit, name)) else {
                continue;
            };

            for (src, dst) in &targets {
                let value = source.get(*src).map(|v| v.trim()).unwrap_or("");
                if !value.is_empty() && row.get(*dst).is_none_or(|v| v.trim().is_empty()) {
                    if row.len() <= *dst {
                        row.resize(*dst + 1, String::new());
                    }
                    row[*dst] = value.to_string();
                }
            }
        }
    }

    Ok(primary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(headers: &[&str], rows: &[&[&str]]) -> CsvDocument {
        CsvDocument {
            file_name: "f.xls".to_string(),
            headers: headers.iter().map(|s| s.to_string()).collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|s| s.to_string()).collect())
                .collect(),
            modified_at: None,
        }
    }

    #[test]
    fn a_join_file_adds_its_columns_to_matching_rows() {
        let primary = doc(
            &["UnitNumber", "FirtLast", "PhoneNumber"],
            &[&["00101", "Ann Lee", "815"], &["00102", "Bo Ray", "816"]],
        );
        let email = doc(
            &["UnitNumber", "FirtLast", "Email"],
            &[&["00102", "bo  ray", "bo@x.com"]],
        );
        let ids = doc(
            &["UnitNumber", "FirtLast", "TenantId"],
            &[&["00101", "ANN LEE", "55"], &["00102", "Bo Ray", "56"]],
        );

        let out = merge_joined(primary, &[email, ids]).unwrap();

        let e = out.header_index("Email").unwrap();
        let t = out.header_index("TenantId").unwrap();
        assert_eq!(out.rows[0][e], "", "no email row for the first tenant");
        assert_eq!(
            out.rows[1][e], "bo@x.com",
            "name match ignores case and spacing"
        );
        assert_eq!(out.rows[0][t], "55");
        assert_eq!(out.rows[1][t], "56");
    }

    #[test]
    fn a_value_already_in_the_main_file_is_never_overwritten() {
        let primary = doc(
            &["UnitNumber", "FirtLast", "Email"],
            &[&["1", "Ann Lee", "keep@x.com"]],
        );
        let email = doc(
            &["UnitNumber", "FirtLast", "Email"],
            &[&["1", "Ann Lee", "other@x.com"]],
        );
        let out = merge_joined(primary, &[email]).unwrap();
        assert_eq!(
            out.rows[0][out.header_index("Email").unwrap()],
            "keep@x.com"
        );
    }

    #[test]
    fn a_blank_in_the_main_file_is_filled() {
        let primary = doc(
            &["UnitNumber", "FirtLast", "Email"],
            &[&["1", "Ann Lee", ""]],
        );
        let email = doc(
            &["UnitNumber", "FirtLast", "Email"],
            &[&["1", "Ann Lee", "a@x.com"]],
        );
        let out = merge_joined(primary, &[email]).unwrap();
        assert_eq!(out.rows[0][out.header_index("Email").unwrap()], "a@x.com");
    }

    #[test]
    fn a_same_unit_different_name_does_not_match() {
        let primary = doc(&["UnitNumber", "FirtLast"], &[&["1", "Ann Lee"]]);
        let ids = doc(
            &["UnitNumber", "FirtLast", "TenantId"],
            &[&["1", "Someone Else", "9"]],
        );
        let out = merge_joined(primary, &[ids]).unwrap();
        assert_eq!(out.rows[0][out.header_index("TenantId").unwrap()], "");
    }

    #[test]
    fn a_join_file_without_the_match_columns_is_refused() {
        let primary = doc(&["UnitNumber", "FirtLast"], &[&["1", "Ann Lee"]]);
        let bad = doc(&["Email"], &[&["a@x.com"]]);
        assert!(merge_joined(primary, &[bad]).is_err());
    }
}
