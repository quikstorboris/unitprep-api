//! Gives each tenant record its real unit number, using a report that says
//! which tenant holds which unit.
//!
//! QuikStor Cloud's `Tenants.csv` describes people but never names a unit,
//! so dedup has to call each record by its tenant ID. Its `Leases.csv` is
//! the missing link: one row per lease with the tenant ID and the unit
//! number. This step reads that link file and replaces the placeholder
//! unit on the main file's rows with the real ones, so findings name
//! "unit 14B" instead of a tenant ID.
//!
//! A tenant with several leases becomes several records, one per unit,
//! each carrying the same contact details, the same shape a QMS export has
//! natively (one row per tenant per unit). The two real exports differ in
//! how `Tenants.csv` already repeats people: Freeland writes one identical
//! row per lease, Davidson Road writes one row per tenant. Both come out
//! the same, because identical repeats are collapsed before the tenant is
//! spread over their units.
//!
//! Like `join`, this is generic: both sides are already in canonical column
//! names (`TenantId`, `UnitNumber`), so nothing here knows about any one
//! vendor.

use std::collections::HashMap;

use anyhow::{bail, Result};
use unitprep_core::csv_document::CsvDocument;

const TENANT_ID: &str = "TenantId";
const UNIT: &str = "UnitNumber";
const NAME: &str = "FirtLast";

/// A report that links tenants to units rather than describing tenants:
/// it has both a tenant ID and a unit but no customer name, so it can't be
/// matched by name the way an ordinary join file is.
pub fn is_unit_link(doc: &CsvDocument) -> bool {
    doc.header_index(TENANT_ID).is_some()
        && doc.header_index(UNIT).is_some()
        && doc.header_index(NAME).is_none()
}

/// Returns `primary` with its `UnitNumber` taken from `link`, one record per
/// tenant per unit.
///
/// A tenant with no lease in `link` keeps its row unchanged, and a lease
/// for a tenant `primary` doesn't have is ignored. When the main file holds
/// several *different* rows for one tenant there is no way to tell which
/// belongs to which lease, so that is refused rather than guessed.
pub fn attach_units(mut primary: CsvDocument, link: &CsvDocument) -> Result<CsvDocument> {
    let (Some(tenant_idx), Some(unit_idx)) =
        (primary.header_index(TENANT_ID), primary.header_index(UNIT))
    else {
        bail!(
            "The main file has no tenant ID or unit column, so '{}' can't say which units each tenant holds",
            link.file_name
        );
    };
    let (Some(l_tenant), Some(l_unit)) = (link.header_index(TENANT_ID), link.header_index(UNIT))
    else {
        bail!(
            "'{}' has no tenant ID or unit column, so it can't say which units each tenant holds",
            link.file_name
        );
    };

    // Each tenant's units in file order. A repeated (tenant, unit) pair, such
    // as a renewed lease on the same unit, is one unit, not two.
    let mut units: HashMap<String, Vec<String>> = HashMap::new();
    for row in &link.rows {
        let tenant = row.get(l_tenant).map(|v| v.trim()).unwrap_or("");
        let unit = row.get(l_unit).map(|v| v.trim()).unwrap_or("");
        if tenant.is_empty() || unit.is_empty() {
            continue;
        }
        let held = units.entry(tenant.to_string()).or_default();
        if !held.iter().any(|u| u == unit) {
            held.push(unit.to_string());
        }
    }

    // The main file's rows per tenant, in order of first appearance, with
    // exact repeats collapsed.
    let mut order: Vec<String> = Vec::new();
    let mut rows_of: HashMap<String, Vec<Vec<String>>> = HashMap::new();
    let mut out: Vec<Vec<String>> = Vec::new();
    for row in primary.rows.drain(..) {
        let tenant = row
            .get(tenant_idx)
            .map(|v| v.trim().to_string())
            .unwrap_or_default();
        if tenant.is_empty() || !units.contains_key(&tenant) {
            // Nothing to attach: leave the row exactly as it was.
            out.push(row);
            continue;
        }
        let rows = rows_of.entry(tenant.clone()).or_insert_with(|| {
            order.push(tenant.clone());
            Vec::new()
        });
        if !rows.contains(&row) {
            rows.push(row);
        }
    }

    // Spread each tenant over their units. Placed after the untouched rows,
    // which is fine: dedup groups by tenant, never by row position.
    for tenant in order {
        let rows = &rows_of[&tenant];
        let held = &units[&tenant];
        if rows.len() == 1 {
            for unit in held {
                let mut row = rows[0].clone();
                set_cell(&mut row, unit_idx, unit);
                out.push(row);
            }
        } else if rows.len() == held.len() {
            for (row, unit) in rows.iter().zip(held) {
                let mut row = row.clone();
                set_cell(&mut row, unit_idx, unit);
                out.push(row);
            }
        } else {
            bail!(
                "The main file has {} different rows for tenant {tenant} but '{}' lists {} unit(s) for them, so the units can't be matched to the rows without guessing",
                rows.len(),
                link.file_name,
                held.len()
            );
        }
    }

    primary.rows = out;
    Ok(primary)
}

fn set_cell(row: &mut Vec<String>, idx: usize, value: &str) {
    if row.len() <= idx {
        row.resize(idx + 1, String::new());
    }
    row[idx] = value.to_string();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(headers: &[&str], rows: &[&[&str]]) -> CsvDocument {
        CsvDocument {
            file_name: "f.csv".to_string(),
            headers: headers.iter().map(|s| s.to_string()).collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().map(|s| s.to_string()).collect())
                .collect(),
            modified_at: None,
        }
    }

    const TENANTS: [&str; 3] = ["TenantId", "UnitNumber", "FirtLast"];
    const LEASES: [&str; 2] = ["TenantId", "UnitNumber"];

    fn units_of(d: &CsvDocument, tenant: &str) -> Vec<String> {
        let (t, u) = (
            d.header_index("TenantId").unwrap(),
            d.header_index("UnitNumber").unwrap(),
        );
        d.rows
            .iter()
            .filter(|r| r[t] == tenant)
            .map(|r| r[u].clone())
            .collect()
    }

    #[test]
    fn a_link_report_is_told_from_a_tenant_report_by_having_no_name() {
        assert!(is_unit_link(&doc(&LEASES, &[])));
        assert!(!is_unit_link(&doc(&TENANTS, &[])));
        assert!(!is_unit_link(&doc(&["UnitNumber", "FirtLast"], &[])));
    }

    /// Davidson Road: one tenant row, several leases.
    #[test]
    fn one_tenant_row_is_spread_over_every_unit_the_tenant_holds() {
        let tenants = doc(&TENANTS, &[&["7", "7", "Ann Lee"], &["8", "8", "Bo Ray"]]);
        let leases = doc(&LEASES, &[&["7", "A1"], &["8", "B2"], &["7", "A2"]]);

        let out = attach_units(tenants, &leases).unwrap();

        assert_eq!(out.rows.len(), 3);
        assert_eq!(units_of(&out, "7"), ["A1", "A2"]);
        assert_eq!(units_of(&out, "8"), ["B2"]);
        let n = out.header_index("FirtLast").unwrap();
        assert!(out
            .rows
            .iter()
            .filter(|r| r[0] == "7")
            .all(|r| r[n] == "Ann Lee"));
    }

    /// Freeland: the tenant row is already repeated once per lease.
    #[test]
    fn identical_repeated_tenant_rows_are_not_multiplied() {
        let tenants = doc(
            &TENANTS,
            &[
                &["7", "7", "Ann Lee"],
                &["7", "7", "Ann Lee"],
                &["8", "8", "Bo Ray"],
            ],
        );
        let leases = doc(&LEASES, &[&["7", "A1"], &["7", "A2"], &["8", "B2"]]);

        let out = attach_units(tenants, &leases).unwrap();

        assert_eq!(out.rows.len(), 3, "2 + 1 leases, not 4 + 1 records");
        assert_eq!(units_of(&out, "7"), ["A1", "A2"]);
    }

    #[test]
    fn a_unit_listed_twice_for_one_tenant_counts_once() {
        let tenants = doc(&TENANTS, &[&["7", "7", "Ann Lee"]]);
        let leases = doc(&LEASES, &[&["7", "A1"], &["7", " A1 "]]);
        let out = attach_units(tenants, &leases).unwrap();
        assert_eq!(units_of(&out, "7"), ["A1"]);
    }

    #[test]
    fn a_tenant_with_no_lease_keeps_its_row_and_an_unknown_lease_is_ignored() {
        let tenants = doc(&TENANTS, &[&["7", "7", "Ann Lee"], &["9", "9", "Cy Poe"]]);
        let leases = doc(&LEASES, &[&["7", "A1"], &["99", "Z9"]]);

        let out = attach_units(tenants, &leases).unwrap();

        assert_eq!(out.rows.len(), 2);
        assert_eq!(units_of(&out, "7"), ["A1"]);
        assert_eq!(
            units_of(&out, "9"),
            ["9"],
            "no lease: placeholder unit stays"
        );
    }

    #[test]
    fn a_lease_row_with_no_unit_is_skipped() {
        let tenants = doc(&TENANTS, &[&["7", "7", "Ann Lee"]]);
        let leases = doc(&LEASES, &[&["7", "  "], &["7", "A1"]]);
        let out = attach_units(tenants, &leases).unwrap();
        assert_eq!(units_of(&out, "7"), ["A1"]);
    }

    #[test]
    fn different_rows_for_one_tenant_that_do_not_line_up_with_the_units_are_refused() {
        let tenants = doc(
            &TENANTS,
            &[&["7", "7", "Ann Lee"], &["7", "7", "Ann Leigh"]],
        );
        let leases = doc(&LEASES, &[&["7", "A1"], &["7", "A2"], &["7", "A3"]]);
        let err = attach_units(tenants, &leases).unwrap_err().to_string();
        assert!(err.contains("without guessing"), "{err}");
    }

    #[test]
    fn different_rows_for_one_tenant_pair_up_with_the_units_in_order() {
        let tenants = doc(
            &TENANTS,
            &[&["7", "7", "Ann Lee"], &["7", "7", "Ann Leigh"]],
        );
        let leases = doc(&LEASES, &[&["7", "A1"], &["7", "A2"]]);
        let out = attach_units(tenants, &leases).unwrap();
        let n = out.header_index("FirtLast").unwrap();
        assert_eq!(
            (out.rows[0][1].as_str(), out.rows[0][n].as_str()),
            ("A1", "Ann Lee")
        );
        assert_eq!(
            (out.rows[1][1].as_str(), out.rows[1][n].as_str()),
            ("A2", "Ann Leigh")
        );
    }

    #[test]
    fn a_main_file_without_a_tenant_id_is_refused() {
        let tenants = doc(&["UnitNumber", "FirtLast"], &[&["1", "Ann Lee"]]);
        let leases = doc(&LEASES, &[&["7", "A1"]]);
        assert!(attach_units(tenants, &leases).is_err());
    }
}
