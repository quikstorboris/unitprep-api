//! SiteLink's tenant-export transform (Directory / Rent Roll reports).

use super::{cell, person_key};
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
}
