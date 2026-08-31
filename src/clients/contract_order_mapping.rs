//! Maps a ✅ Contract Order run's form fields into the shape
//! `clients::repository` writes to Postgres.
//!
//! A given facility having no Contract Order run at all is expected --
//! confirmed by Boris this is inconsistent sales-territory/process
//! behavior, not a data gap to chase (see [[Process Street Integration
//! — Kickoff & Findings]] in the vault).
//!
//! Nothing in this workflow is sensitive -- order/rate/POC/shipping/
//! payment-form-checklist data, no SSN/bank-account/government-ID data
//! the way New Merchant Account has. Only `migrating_from_system` is
//! promoted to a named column for now (the field Boris flagged as the
//! operationally important one: which legacy system the client is
//! migrating off of onto QMS) -- everything else stays in
//! `raw_ps_snapshot` verbatim rather than being hand-modeled ahead of
//! an actual OO UI need for it. The real run has ~99 fields covering
//! rate quoted, processor, onboarding POC, go-live date, shipping/
//! payment-form checklists -- all still recoverable from the snapshot
//! if a future tab needs to surface them as real columns.
//!
//! **How this workflow's runs were actually found, worth remembering**:
//! `GET /workflow-runs` defaults to `status=Active` only. Contract
//! Order runs get marked `Completed` once the order is processed, so
//! an Active-only search finds essentially none of them -- confirmed
//! directly: searching by name for two real, known clients (Tri County
//! Mini Storage, Dubuqueland Mini Storage) across the "Active" runs
//! found nothing; both turned up immediately once `status=Completed`
//! was queried explicitly. Any future search/listing code must query
//! across Active + Completed + Archived (not Deleted) to get a true
//! inventory for any workflow, not just this one -- see the vault
//! Gotchas entry this earned.

use crate::clients::fields::value_for;
use crate::process_street::FormField;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MappedContractOrder {
    pub migrating_from_system: Option<String>,
}

pub fn map_contract_order_fields(fields: &[FormField]) -> MappedContractOrder {
    MappedContractOrder {
        migrating_from_system: value_for(
            fields,
            "What_software_are_they_currently_using_to_manage_this_facility?",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real Contract Order data for two real clients -- safe to commit
    // verbatim, this workflow has no sensitive fields at all (checked
    // directly: no SSN/bank-account/government-ID field exists in
    // either real run's ~99 fields).
    const TRI_COUNTY_FIELDS: &str = include_str!("testdata/tri_county_contract_order_fields.json");
    const DUBUQUELAND_FIELDS: &str = include_str!("testdata/dubuqueland_contract_order_fields.json");

    fn fields(json: &str) -> Vec<FormField> {
        serde_json::from_str(json).expect("fixture must parse as Vec<FormField>")
    }

    #[test]
    fn parses_both_real_fixtures_without_panicking() {
        map_contract_order_fields(&fields(TRI_COUNTY_FIELDS));
        map_contract_order_fields(&fields(DUBUQUELAND_FIELDS));
    }

    #[test]
    fn migrating_from_system_is_none_on_both_real_examples_checked_so_far() {
        // Neither real client examined this session had this field
        // answered -- confirms the field key is correct and the mapper
        // handles an all-blank real run cleanly, NOT that a real
        // filled example has been seen yet. Update this test the first
        // time one is.
        assert_eq!(
            map_contract_order_fields(&fields(TRI_COUNTY_FIELDS)).migrating_from_system,
            None
        );
        assert_eq!(
            map_contract_order_fields(&fields(DUBUQUELAND_FIELDS)).migrating_from_system,
            None
        );
    }
}
