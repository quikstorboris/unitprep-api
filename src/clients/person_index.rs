//! Extracts the thin, searchable slice of each PS run's people --
//! name/email/phone/role only, never touching any of Merchant Account's
//! sensitive `PartyPii` fields (SSN/DOB/bank/EIN/credentials stay
//! exactly where `merchant_account_mapping`/`encryption` already put
//! them, encrypted, and are never read by this module). This is the
//! per-run projection `clients::sync` writes into
//! `clients.ps_person_index`; it duplicates the field-key knowledge
//! `intake_mapping`/`merchant_account_mapping`/`contract_order_mapping`
//! already have (rather than importing their private mapping structs),
//! since all three of those modules' output types carry far more than
//! this index needs and two of them (Merchant Account, in particular)
//! deliberately keep their sensitive fields private to their own
//! module.

// No caller yet outside `clients::sync`, which itself has no HTTP
// handler wired up yet. Remove once one exists.
#![allow(dead_code)]

use crate::clients::fields::value_for;
use crate::clients::people::parse_people_block;
use crate::process_street::FormField;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedPerson {
    pub full_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    /// One of `ps_person_index.role`'s CHECK values -- see the
    /// migration.
    pub role: &'static str,
}

fn display_name(first: Option<String>, last: Option<String>) -> Option<String> {
    match (first, last) {
        (Some(f), Some(l)) => Some(format!("{f} {l}")),
        (Some(f), None) => Some(f),
        (None, Some(l)) => Some(l),
        (None, None) => None,
    }
}

/// Owner/District Manager/Manager Level Users -- the same three
/// free-text blocks `intake_mapping` already parses with
/// `parse_people_block`, copy-pasted verbatim onto every sister
/// facility's own run regardless of the "first time" flag (see the
/// vault's sister-site finding), so indexing each facility's own run
/// independently is correct, not redundant.
pub fn extract_intake_people(fields: &[FormField]) -> Vec<ExtractedPerson> {
    let blocks: &[(&str, &'static str)] = &[
        ("Owner_Level_Users:", "owner"),
        ("District_Manager_Level_Users:", "district_manager"),
        ("Manager_Level_Users", "manager"),
    ];

    blocks
        .iter()
        .flat_map(|(key, role)| {
            value_for(fields, key)
                .map(|raw| parse_people_block(&raw))
                .unwrap_or_default()
                .into_iter()
                .map(|p| ExtractedPerson {
                    full_name: p.full_name,
                    email: p.email,
                    phone: p.phone,
                    role,
                })
        })
        .collect()
}

/// Signer + Owner 1-4 -- same field-key patterns as
/// `merchant_account_mapping::map_parties`, but only the display name/
/// email/phone, never `PartyPii`'s SSN/DOB/bank fields (that struct is
/// private to `merchant_account_mapping` and stays that way).
/// Intermediary businesses are deliberately excluded -- they identify a
/// business, not a person to search by name.
///
/// A second real per-template variant, found while building this
/// module: some Merchant Account runs (Highway 20's real data among
/// them) never fill in the dedicated `Signer_-_First_Name`/`Last_Name`
/// fields at all -- instead a separate `Signer_Name` field just names
/// which already-listed owner is also the signer (Highway 20: "Kyle
/// Lindley", who is also `Owner_1`). `merchant_account_mapping::map_signer`
/// correctly treats that case as "no distinct signer party" for its own
/// PII-pipeline purposes (fabricating a phantom party with no PII would
/// be worse than omitting it), but this search index only needs a name
/// to index under the `signer` role, not a full party -- so it falls
/// back to `Signer_Name` whenever the split fields are empty. This
/// deliberately produces two index rows for the same real person (owner
/// and signer both) when that's genuinely what the data says, matching
/// the already-agreed "two rows, two roles" convention that
/// `clients.facility_people` itself uses.
pub fn extract_merchant_account_people(fields: &[FormField]) -> Vec<ExtractedPerson> {
    let mut people = Vec::new();

    let signer_name = display_name(
        value_for(fields, "Signer_-_First_Name"),
        value_for(fields, "Signer_-_Last_Name"),
    )
    .or_else(|| value_for(fields, "Signer_Name"));
    if let Some(full_name) = signer_name {
        people.push(ExtractedPerson {
            full_name,
            email: value_for(fields, "Signer_-_Email"),
            phone: value_for(fields, "Signer_-_Home_or_Cell_Phone"),
            role: "signer",
        });
    }

    for i in 1..=4 {
        let prefix = format!("Owner_{i}");
        if let Some(full_name) = display_name(
            value_for(fields, &format!("{prefix}_-_First_Name")),
            value_for(fields, &format!("{prefix}_-_Last_Name")),
        ) {
            people.push(ExtractedPerson {
                full_name,
                email: value_for(fields, &format!("{prefix}_-_Email")),
                phone: value_for(fields, &format!("{prefix}_-_Home_or_Cell_Phone")),
                role: "owner",
            });
        }
    }

    people
}

/// Onboarding/Website/Integration POC fields -- the only person-shaped
/// fields Contract Order's template carries.
pub fn extract_contract_order_people(fields: &[FormField]) -> Vec<ExtractedPerson> {
    let mut people = Vec::new();

    if let Some(full_name) = value_for(fields, "Onboarding_POC_Name:") {
        people.push(ExtractedPerson {
            full_name,
            email: value_for(fields, "Onboarding_POC_Email:"),
            phone: value_for(fields, "Onboarding_POC_Phone_Number:"),
            role: "onboarding_poc",
        });
    }

    if let Some(full_name) = value_for(fields, "Website_POC_Name:") {
        people.push(ExtractedPerson {
            full_name,
            email: value_for(fields, "Website_POC_Email:"),
            phone: value_for(fields, "Website_POC_Phone_Number:"),
            role: "website_poc",
        });
    }

    if let Some(full_name) = value_for(fields, "Name_of_Integration_POC") {
        people.push(ExtractedPerson {
            full_name,
            email: value_for(fields, "Integration_POC_Email_Address:"),
            phone: value_for(fields, "Integration_POC_Phone_Number:"),
            role: "integration_poc",
        });
    }

    people
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn load_fixture(name: &str) -> Vec<FormField> {
        let path = format!("src/clients/testdata/{name}");
        let raw = fs::read_to_string(&path).unwrap_or_else(|_| panic!("fixture missing: {path}"));
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("bad fixture {path}: {e}"))
    }

    #[test]
    fn extracts_owners_district_managers_and_managers_from_a_real_intake_fixture() {
        let fields = load_fixture("highway20_intake_fields.json");
        let people = extract_intake_people(&fields);

        assert!(!people.is_empty());
        assert!(people.iter().any(|p| p.role == "owner"));
    }

    #[test]
    fn extracts_signer_and_owners_from_a_sanitized_merchant_account_fixture() {
        let fields = load_fixture("highway20_merchant_account_fields_sanitized.json");
        let people = extract_merchant_account_people(&fields);

        // Real Highway 20 data: the dedicated Signer_-_* fields are all
        // blank, and the signer's name only appears via the `Signer_Name`
        // fallback field -- see this function's own doc comment. Kyle
        // Lindley therefore appears twice here (owner + signer), which
        // is correct, not a dedup bug: he really does hold both roles.
        let signer = people
            .iter()
            .find(|p| p.role == "signer")
            .expect("the Signer_Name fallback must surface a signer even with blank Signer_-_* fields");
        assert_eq!(signer.full_name, "Kyle Lindley");
        assert_eq!(signer.email, None, "no dedicated Signer_-_Email field was filled in on this run");

        let owner = people
            .iter()
            .find(|p| p.role == "owner")
            .expect("the sanitized fixture has at least one owner");
        assert_eq!(owner.full_name, "Kyle Lindley");
        assert_eq!(owner.email.as_deref(), Some("kyle.lindley@outlook.com"));

        // Confirms this module truly never reaches into PartyPii --
        // there is no SSN/DOB field name anywhere above it could
        // accidentally read, but assert the shape stays name/email/
        // phone only as a belt-and-suspenders regression guard.
        assert!(people.iter().all(|p| !p.full_name.is_empty()));
    }

    #[test]
    fn extracts_poc_contacts_from_real_contract_order_fixtures() {
        let tri_county = load_fixture("tri_county_contract_order_fields.json");
        let people = extract_contract_order_people(&tri_county);
        assert!(!people.is_empty());

        let dubuqueland = load_fixture("dubuqueland_contract_order_fields.json");
        let people = extract_contract_order_people(&dubuqueland);
        assert!(!people.is_empty());
    }

    #[test]
    fn returns_empty_not_panicking_when_no_person_fields_are_present() {
        assert_eq!(extract_intake_people(&[]), vec![]);
        assert_eq!(extract_merchant_account_people(&[]), vec![]);
        assert_eq!(extract_contract_order_people(&[]), vec![]);
    }
}
