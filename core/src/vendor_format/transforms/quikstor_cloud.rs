//! QuikStor Cloud's tenant-export transform.

use super::{cell, person_key};
use crate::csv_document::CsvDocument;

/// Export sentinels QuikStor Cloud writes into `Email` when a tenant has
/// none on file. Compared trim+lowercased. Left in place they would read
/// as a real address shared by every such tenant (a false "related
/// tenants" signal) rather than as a blank.
const QUIKSTOR_CLOUD_EMAIL_SENTINELS: &[&str] = &["#noemail", "notprovided@non.com"];

/// QuikStor Cloud's tenant export has no single name column and no
/// single phone column, which dedup's pipeline needs, so this derives
/// them (appended under their canonical names, picked up by
/// `apply_field_mapping`'s ordinary identity entries):
///
/// - `FirtLast`: see `person_key` (first + last, company fallback).
/// - `PhoneNumber`/`PhoneNumberPrefix`: the first non-blank of
///   Cell, Home, Work (each with its own prefix), so a tenant whose
///   phone sits in a different slot on a second lease isn't flagged as
///   a mismatch against blank.
///
/// Also blanks `Email` in place where it holds one of
/// `QUIKSTOR_CLOUD_EMAIL_SENTINELS`. Every source column is optional;
/// one that's absent contributes a blank rather than an error.
pub(super) fn derive_quikstor_cloud_tenant_fields(document: &CsvDocument) -> CsvDocument {
    let email_idx = document.header_index("Email");

    let mut headers = document.headers.clone();
    headers.push("FirtLast".to_string());
    headers.push("PhoneNumber".to_string());
    headers.push("PhoneNumberPrefix".to_string());

    let rows = document
        .rows
        .iter()
        .map(|row| {
            let mut row = row.clone();

            let first_last = person_key(
                &cell(document, &row, "FirstName"),
                &cell(document, &row, "LastName"),
                &cell(document, &row, "CompanyName"),
            );

            let (phone, prefix) = [
                ("CellPhoneNumber", "CellPhoneNumberPrefix"),
                ("HomePhoneNumber", "HomePhoneNumberPrefix"),
                ("WorkPhoneNumber", "WorkPhoneNumberPrefix"),
            ]
            .iter()
            .map(|(number, prefix)| (cell(document, &row, number), cell(document, &row, prefix)))
            .find(|(number, _)| !number.is_empty())
            .unwrap_or_default();

            if let Some(idx) = email_idx {
                let is_sentinel = row.get(idx).is_some_and(|v| {
                    QUIKSTOR_CLOUD_EMAIL_SENTINELS.contains(&v.trim().to_lowercase().as_str())
                });
                if is_sentinel {
                    row[idx].clear();
                }
            }

            row.push(first_last);
            row.push(phone);
            row.push(prefix);
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

    fn qcloud_doc(rows: Vec<Vec<&str>>) -> CsvDocument {
        CsvDocument {
            file_name: "Tenants.csv".to_string(),
            headers: [
                "Email",
                "FirstName",
                "MiddleName",
                "LastName",
                "CompanyName",
                "CellPhoneNumberPrefix",
                "CellPhoneNumber",
                "HomePhoneNumberPrefix",
                "HomePhoneNumber",
                "WorkPhoneNumberPrefix",
                "WorkPhoneNumber",
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
    fn quikstor_cloud_name_key_collapses_whitespace_and_ignores_middle_name() {
        let doc = qcloud_doc(vec![
            vec![
                "a@x.com",
                "Michelle ",
                "",
                " Proulx",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
            ],
            vec![
                "a@x.com", "Jerry", "Gene", "Morrison", "", "", "", "", "", "", "",
            ],
        ]);
        let out = derive_quikstor_cloud_tenant_fields(&doc);
        assert_eq!(derived(&out, 0, "FirtLast"), "Michelle Proulx");
        assert_eq!(derived(&out, 1, "FirtLast"), "Jerry Morrison");
    }

    #[test]
    fn quikstor_cloud_name_key_falls_back_to_company_when_names_are_blank() {
        let doc = qcloud_doc(vec![vec![
            "",
            " ",
            "",
            "",
            "Acme  Storage LLC",
            "",
            "",
            "",
            "",
            "",
            "",
        ]]);
        let out = derive_quikstor_cloud_tenant_fields(&doc);
        assert_eq!(derived(&out, 0, "FirtLast"), "Acme Storage LLC");
    }

    #[test]
    fn quikstor_cloud_phone_prefers_cell_then_home_then_work_with_its_own_prefix() {
        let doc = qcloud_doc(vec![
            vec!["", "A", "", "B", "", "+1", "111", "+1", "222", "+1", "333"],
            vec!["", "A", "", "B", "", "", "", "+44", "222", "+1", "333"],
            vec!["", "A", "", "B", "", "", "", "", "", "+1", "333"],
            vec!["", "A", "", "B", "", "", "", "", "", "", ""],
        ]);
        let out = derive_quikstor_cloud_tenant_fields(&doc);
        assert_eq!(derived(&out, 0, "PhoneNumber"), "111");
        assert_eq!(derived(&out, 1, "PhoneNumber"), "222");
        assert_eq!(derived(&out, 1, "PhoneNumberPrefix"), "+44");
        assert_eq!(derived(&out, 2, "PhoneNumber"), "333");
        assert_eq!(derived(&out, 3, "PhoneNumber"), "");
        assert_eq!(derived(&out, 3, "PhoneNumberPrefix"), "");
    }

    #[test]
    fn quikstor_cloud_blanks_email_sentinels_but_keeps_real_addresses() {
        let doc = qcloud_doc(vec![
            vec!["#NoEmail", "A", "", "B", "", "", "", "", "", "", ""],
            vec![
                " notprovided@non.com ",
                "A",
                "",
                "B",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
            ],
            vec!["real@x.com", "A", "", "B", "", "", "", "", "", "", ""],
        ]);
        let out = derive_quikstor_cloud_tenant_fields(&doc);
        assert_eq!(derived(&out, 0, "Email"), "");
        assert_eq!(derived(&out, 1, "Email"), "");
        assert_eq!(derived(&out, 2, "Email"), "real@x.com");
    }

    #[test]
    fn quikstor_cloud_transform_tolerates_missing_source_columns() {
        let doc = CsvDocument {
            file_name: "x.csv".to_string(),
            headers: vec!["FirstName".to_string()],
            rows: vec![vec!["Solo".to_string()]],
            modified_at: None,
        };
        let out = derive_quikstor_cloud_tenant_fields(&doc);
        assert_eq!(derived(&out, 0, "FirtLast"), "Solo");
        assert_eq!(derived(&out, 0, "PhoneNumber"), "");
    }
}
