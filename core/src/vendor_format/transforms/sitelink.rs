//! SiteLink's tenant-export transform (Directory / Rent Roll reports).

use super::{cell, collapse_whitespace, person_key};
use crate::csv_document::CsvDocument;

/// SiteLink's Directory and Rent Roll reports have no single name column
/// and carry the home and mobile phone in separate columns, which
/// dedup's pipeline needs as one, so this derives them (appended under
/// their canonical names, picked up by `apply_field_mapping`'s ordinary
/// identity entries):
///
/// - `FirtLast`: see `person_key` over `sFName`/`sLName`/`sCompany`.
///   `TenantName` ("Last, First") is deliberately not used: Rent Roll
///   has no such column and the split name fields are the source of it.
/// - `PhoneNumber`: `sPhone`, falling back to `sMobile` when blank.
///
/// Rent Roll lists every unit, vacant ones included, as a row with no
/// `LedgerID` and no tenant. Those rows are dropped here: a vacant unit
/// is not a tenant record and would only add blank-name rows to the
/// report. The Directory report has none, so this is a no-op there.
/// With no `LedgerID` column at all, every row is kept. Every source
/// column is optional; one that's absent contributes a blank.
pub(super) fn derive_sitelink_tenant_fields(document: &CsvDocument) -> CsvDocument {
    let ledger_idx = document.header_index("LedgerID");

    let mut headers = document.headers.clone();
    headers.push("FirtLast".to_string());
    headers.push("PhoneNumber".to_string());

    let rows = document
        .rows
        .iter()
        .filter(|row| match ledger_idx {
            Some(idx) => row.get(idx).is_some_and(|v| !v.trim().is_empty()),
            None => true,
        })
        .map(|row| {
            let mut row = row.clone();

            let first_last = person_key(
                &cell(document, &row, "sFName"),
                &cell(document, &row, "sLName"),
                &cell(document, &row, "sCompany"),
            );

            let mut phone = cell(document, &row, "sPhone");
            if phone.is_empty() {
                phone = cell(document, &row, "sMobile");
            }

            row.push(first_last);
            row.push(phone);
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

/// SiteLink prices and groups units by unit type x size (the Price List's
/// grain), but its Custom Unit Report carries them as two separate
/// columns and has no group column, which Group Prep needs as one. This
/// appends `UnitGroup` = `"{Type} {UnitSize}"` (whitespace collapsed),
/// e.g. `"Self Storage 10x20"`; the original columns are kept so the
/// vendor's signature still matches afterwards. If a `UnitGroup` column
/// is already present (the transform has run before) the document is
/// returned unchanged, so applying it twice is harmless.
pub(super) fn derive_sitelink_unit_group(document: &CsvDocument) -> CsvDocument {
    if document.header_index("UnitGroup").is_some() {
        return document.clone();
    }

    let mut headers = document.headers.clone();
    headers.push("UnitGroup".to_string());

    let rows = document
        .rows
        .iter()
        .map(|row| {
            let mut row = row.clone();
            let group = collapse_whitespace(&format!(
                "{} {}",
                cell(document, &row, "Type"),
                cell(document, &row, "UnitSize")
            ));
            row.push(group);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sitelink_doc(rows: Vec<Vec<&str>>) -> CsvDocument {
        CsvDocument {
            file_name: "Rent Roll.xlsx".to_string(),
            headers: [
                "sUnitName",
                "LedgerID",
                "TenantID",
                "sFName",
                "sLName",
                "sCompany",
                "sPhone",
                "sMobile",
            ]
            .iter()
            .map(|h| h.to_string())
            .collect(),
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(String::from).collect())
                .collect(),
            modified_at: None,
        }
    }

    fn derived(doc: &CsvDocument, row: usize, header: &str) -> String {
        let idx = doc.header_index(header).expect("derived column present");
        doc.rows[row][idx].clone()
    }

    #[test]
    fn sitelink_name_key_uses_first_and_last_with_company_fallback() {
        let doc = sitelink_doc(vec![
            vec!["A01", "1", "10", " Lope & Tony ", "Gonzalez", "", "", ""],
            vec!["A02", "2", "11", "", "", "Acme  Lawn Care", "", ""],
        ]);
        let out = derive_sitelink_tenant_fields(&doc);
        assert_eq!(derived(&out, 0, "FirtLast"), "Lope & Tony Gonzalez");
        assert_eq!(derived(&out, 1, "FirtLast"), "Acme Lawn Care");
    }

    #[test]
    fn sitelink_phone_falls_back_to_mobile_only_when_phone_is_blank() {
        let doc = sitelink_doc(vec![
            vec![
                "A01",
                "1",
                "10",
                "A",
                "B",
                "",
                "(575) 111-1111",
                "(575) 222-2222",
            ],
            vec!["A02", "2", "11", "A", "B", "", "", "(575) 222-2222"],
            vec!["A03", "3", "12", "A", "B", "", "", ""],
        ]);
        let out = derive_sitelink_tenant_fields(&doc);
        assert_eq!(derived(&out, 0, "PhoneNumber"), "(575) 111-1111");
        assert_eq!(derived(&out, 1, "PhoneNumber"), "(575) 222-2222");
        assert_eq!(derived(&out, 2, "PhoneNumber"), "");
    }

    #[test]
    fn sitelink_drops_vacant_unit_rows_that_have_no_ledger() {
        let doc = sitelink_doc(vec![
            vec!["A01", "1", "10", "A", "B", "", "", ""],
            vec!["V01", "", "", "", "", "", "", ""],
            vec!["A02", " ", "", "", "", "", "", ""],
        ]);
        let out = derive_sitelink_tenant_fields(&doc);
        assert_eq!(out.rows.len(), 1);
        assert_eq!(derived(&out, 0, "sUnitName"), "A01");
    }

    #[test]
    fn sitelink_keeps_every_row_when_there_is_no_ledger_column() {
        let doc = CsvDocument {
            file_name: "x.xlsx".to_string(),
            headers: vec!["sFName".to_string()],
            rows: vec![vec!["Solo".to_string()], vec![String::new()]],
            modified_at: None,
        };
        let out = derive_sitelink_tenant_fields(&doc);
        assert_eq!(out.rows.len(), 2);
        assert_eq!(derived(&out, 0, "FirtLast"), "Solo");
        assert_eq!(derived(&out, 0, "PhoneNumber"), "");
    }

    fn unit_report(rows: Vec<Vec<&str>>) -> CsvDocument {
        CsvDocument {
            file_name: "Custom Unit Report.xlsx".to_string(),
            headers: [
                "UnitName",
                "Type",
                "Width",
                "Length",
                "UnitSize",
                "StandardRate",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            rows: rows
                .into_iter()
                .map(|r| r.into_iter().map(String::from).collect())
                .collect(),
            modified_at: None,
        }
    }

    #[test]
    fn sitelink_unit_group_joins_type_and_size() {
        let out = derive_sitelink_unit_group(&unit_report(vec![
            vec!["A01", "Enclosed RV Park", "12.5", "42", "12.5x42", "125"],
            vec!["B02", "Self Storage", "10", "20", "10x20", "90"],
        ]));
        let idx = out.header_index("UnitGroup").unwrap();
        assert_eq!(out.rows[0][idx], "Enclosed RV Park 12.5x42");
        assert_eq!(out.rows[1][idx], "Self Storage 10x20");
        assert_eq!(out.header_index("UnitName"), Some(0), "originals are kept");
    }

    #[test]
    fn sitelink_unit_group_collapses_stray_whitespace_and_tolerates_a_blank_part() {
        let out = derive_sitelink_unit_group(&unit_report(vec![
            vec!["A01", "  Small   Outdoor ", "10", "20", " 10x20 ", "50"],
            vec!["A02", "Parking", "10", "20", "", "50"],
        ]));
        let idx = out.header_index("UnitGroup").unwrap();
        assert_eq!(out.rows[0][idx], "Small Outdoor 10x20");
        assert_eq!(out.rows[1][idx], "Parking");
    }

    #[test]
    fn sitelink_unit_group_is_a_no_op_the_second_time() {
        let once = derive_sitelink_unit_group(&unit_report(vec![vec![
            "A01", "Parking", "10", "20", "10x20", "50",
        ]]));
        let twice = derive_sitelink_unit_group(&once);
        assert_eq!(once.headers, twice.headers);
        assert_eq!(once.rows, twice.rows);
    }
}
