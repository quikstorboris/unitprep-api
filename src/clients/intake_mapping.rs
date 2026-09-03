//! Maps a 🚂 Intake / Progress run's form fields into the shapes
//! `clients::repository` writes to Postgres. Pure functions -- no I/O,
//! no encryption (nothing in this workflow is sensitive enough to need
//! it, unlike `merchant_account_mapping`), fully testable against
//! captured PS JSON.
//!
//! Every value is kept as PS's own raw text -- Boris's explicit Phase 1
//! call, see the vault schema doc's "Phase 1 model: raw text" section.
//! The only judgment call this module makes is a *known-type tag*
//! (which fee/delinquency-step category a field belongs to) so values
//! land in the right slot in the OO UI -- it never decomposes a value
//! into a decimal/boolean/day-count.

// Phase 1 only -- no HTTP handler calls into `clients::*` yet. Remove
// once a real caller exists.
#![allow(dead_code)]

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::clients::fields::{value_for, values_for};
use crate::clients::people::{parse_people_block, ParsedPerson};
use crate::process_street::FormField;

/// PS sends dates as full ISO-8601 timestamps ("2026-08-19T15:00:00.000Z")
/// even though only the date half means anything here (a Go Live Date
/// has no meaningful time-of-day). Best-effort: an unparseable value is
/// dropped rather than failing the whole ingestion over one bad date.
fn parse_ps_date(fields: &[FormField], key: &str) -> Option<NaiveDate> {
    let raw = value_for(fields, key)?;
    let date_part = raw.split('T').next().unwrap_or(&raw);
    NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()
}

fn parse_units_count(fields: &[FormField], key: &str) -> Option<i32> {
    value_for(fields, key)?.parse::<i32>().ok()
}

/// `Some(true)`/`Some(false)` for a real "Yes"/"No" answer (matched
/// case-insensitively -- real PS values seen so far are exactly "Yes"/
/// "No", but this is a free-text-backed Select field, not a strict
/// enum), `None` when unanswered. See `MappedIntakeRun::is_first_time`'s
/// own doc comment for why this isn't just a `bool`.
fn parse_is_first_time(fields: &[FormField]) -> Option<bool> {
    let raw = value_for(fields, "Is_this_their_first_time_filling_out_this_form?")?;
    match raw.to_lowercase().as_str() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

/// Also the wire shape for the confirmation-screen preview/create round
/// trip (`api::clients_preview`/`api::clients_create`) -- every field
/// here is editable on that screen, so no separate DTO is worth having.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MappedCompany {
    pub legal_name: Option<String>,
    pub corporate_email: Option<String>,
    pub corporate_phone: Option<String>,
    pub corporate_address_street: Option<String>,
    pub corporate_address_city: Option<String>,
    pub corporate_address_state: Option<String>,
    pub corporate_address_zip: Option<String>,
    /// PS's own `Company_Subdomain:` -- captured on the "first time"
    /// facility's run, same sister-site pattern as the other corporate
    /// fields above (see the vault's sister-site finding).
    pub subdomain: Option<String>,
    /// The Company page's "Financial Information" section (Phase 4) --
    /// confirmed 2026-09-03 to live on Intake, not New Merchant Account
    /// as originally assumed in the vault's own design note. Same
    /// "first-time facility" convention as every other field in this
    /// struct: PS asks these once per company, not per facility.
    pub accepted_payment_methods: Option<String>,
    pub accounting_basis: Option<String>,
    pub payment_scheme: Option<String>,
    /// PS's own `Are_they_currently_offering_insurance/protection_to_
    /// their_tenants?` -- raw text, not boolean, same convention as
    /// every other yes/no PS field in this schema.
    pub offers_tenant_insurance_raw: Option<String>,
    pub insurance_provider: Option<String>,
}

/// Also the wire shape for the confirmation-screen preview response
/// (`api::clients_preview`) -- `Serialize` only, deliberately no
/// `Deserialize`: `go_live_date` is shown on that screen (labeled
/// "Original Go Live Date") but never editable there, so the create
/// request carries a narrower `api::clients_create::EditableFacilityFields`
/// instead of this whole struct -- making it structurally impossible to
/// submit an edited go_live_date, not just a convention to follow.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MappedFacility {
    pub name: Option<String>,
    pub street_address: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub zip: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub units_count: Option<i32>,
    pub primary_storage_offering: Option<String>,
    pub previous_pms: Option<String>,
    pub access_control_system: Option<String>,
    pub go_live_date: Option<NaiveDate>,
    pub dropbox_folder_url: Option<String>,
    /// PS's own per-facility `Facility_Subdomain:` -- distinct from
    /// `MappedCompany::subdomain`, which is the company-level one. Real
    /// Highway 20 data has both, with different values.
    pub subdomain: Option<String>,
    /// PS's own `Does_the_Facility_Subdomain_already_exist_in_QMS?` --
    /// kept as raw text (not boolean), same convention as every other
    /// yes/no field in Facility Policies (`sales_tax_applies_raw` etc.).
    pub subdomain_exists_in_qms_raw: Option<String>,
    /// PS's own `Facility_Email_Address:` -- the QMS-associated system
    /// email, distinct from `email` above (the general facility contact
    /// address, PS's `What_is_the_facility_email_address?`). Real
    /// Highway 20 data has both, with different values.
    pub system_email: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MappedFee {
    /// One of `security_deposit`/`nsf_chargeback`/`move_in_admin`/
    /// `transfer`/`cleaning`/`other` -- matches `policy_fees.fee_type`'s
    /// CHECK constraint exactly.
    pub fee_type: &'static str,
    pub label: Option<String>,
    pub raw_value: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MappedTaxes {
    pub sales_tax_applies_raw: Option<String>,
    pub sales_tax_rate_raw: Option<String>,
    pub rent_tax_applies_raw: Option<String>,
    pub rent_tax_rate_raw: Option<String>,
    pub rent_tax_applies_to_all_units_raw: Option<String>,
    pub other_one_time_taxes_raw: Option<String>,
    pub other_recurring_taxes_raw: Option<String>,
}

impl MappedTaxes {
    fn is_empty(&self) -> bool {
        self.sales_tax_applies_raw.is_none()
            && self.sales_tax_rate_raw.is_none()
            && self.rent_tax_applies_raw.is_none()
            && self.rent_tax_rate_raw.is_none()
            && self.rent_tax_applies_to_all_units_raw.is_none()
            && self.other_one_time_taxes_raw.is_none()
            && self.other_recurring_taxes_raw.is_none()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MappedDelinquencyStep {
    pub step_order: i32,
    /// `late_fee`/`pre_lien`/`lien`/`cut_lock`/`auction`/`notice`/`other`
    /// -- matches `policy_delinquency_steps.step_type`'s CHECK constraint.
    pub step_type: &'static str,
    pub raw_value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MappedCoverageTier {
    pub tier_number: i32,
    pub total_coverage_amount_raw: Option<String>,
    pub cost_to_tenant_raw: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MappedCommission {
    pub commission_type_raw: Option<String>,
    pub dollar_amount_raw: Option<String>,
    pub percent_amount_raw: Option<String>,
}

impl MappedCommission {
    fn is_empty(&self) -> bool {
        self.commission_type_raw.is_none()
            && self.dollar_amount_raw.is_none()
            && self.percent_amount_raw.is_none()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MappedIntakeRun {
    /// PS's own `Is_this_their_first_time_filling_out_this_form?` --
    /// `None` when the field itself is unanswered/absent, distinct from
    /// `Some(false)` (a real "No" answer). Deliberately kept off
    /// `MappedCompany` itself, even though it gates that whole struct's
    /// real answers -- `MappedCompany` is the wire shape a manager edits
    /// and resubmits on Create (see that struct's own doc comment), and
    /// this flag isn't an editable company field, just a signal for
    /// picking which selected run should seed the Company section on
    /// the confirmation screen (`pickCompanySourceRun` in
    /// `unitprep-ui`). See the vault's sister-site writeup for why this
    /// gate exists at all.
    pub is_first_time: Option<bool>,
    pub company: MappedCompany,
    pub facility: MappedFacility,
    pub fees: Vec<MappedFee>,
    pub taxes: Option<MappedTaxes>,
    pub delinquency_steps: Vec<MappedDelinquencyStep>,
    pub coverage_tiers: Vec<MappedCoverageTier>,
    pub commission: Option<MappedCommission>,
    pub specials_raw_text: Option<String>,
    pub owners: Vec<ParsedPerson>,
    pub district_managers: Vec<ParsedPerson>,
    pub managers: Vec<ParsedPerson>,
}

/// One named fee -> `(fee_type, PS field key)`. "Any Other Fees" is
/// deliberately last and untouched as one verbatim blob -- see this
/// module's doc comment on why it's never split apart.
const NAMED_FEE_FIELDS: &[(&str, &str)] = &[
    ("security_deposit", "Security_Deposit:"),
    ("nsf_chargeback", "NSF_/_Chargeback_Fee:"),
    ("move_in_admin", "Move-In_Admin_Fee:"),
    ("transfer", "Transfer_Fee:"),
    ("cleaning", "Cleaning_Fee:"),
];

fn map_fees(fields: &[FormField]) -> Vec<MappedFee> {
    let mut fees: Vec<MappedFee> = NAMED_FEE_FIELDS
        .iter()
        .filter_map(|(fee_type, key)| {
            value_for(fields, key).map(|raw_value| MappedFee {
                fee_type,
                label: None,
                raw_value,
            })
        })
        .collect();

    if let Some(raw_value) = value_for(fields, "Any_Other_Fees:") {
        fees.push(MappedFee {
            fee_type: "other",
            label: Some("Any Other Fees".to_string()),
            raw_value,
        });
    }

    fees
}

fn map_taxes(fields: &[FormField]) -> Option<MappedTaxes> {
    let taxes = MappedTaxes {
        sales_tax_applies_raw: value_for(fields, "Is_this_facility_subject_to_sales_tax_on_retail_items?"),
        sales_tax_rate_raw: value_for(fields, "Sales_Tax_Rate"),
        rent_tax_applies_raw: value_for(fields, "Does_the_facility_use_a_Rent_Tax?"),
        rent_tax_rate_raw: value_for(fields, "What_is_the_rate_of_this_Rent_Tax?"),
        rent_tax_applies_to_all_units_raw: value_for(
            fields,
            "Does_this_Rent_Tax_apply_to_all_units_at_this_facility?",
        ),
        other_one_time_taxes_raw: value_for(
            fields,
            "Additional_One-Time_Taxes_-_Name_/_Rate_/_Attribute_Payable:",
        ),
        other_recurring_taxes_raw: value_for(fields, "Name(s)_&_Rate(s)_of_Add'l_Recurring_Taxes:"),
    };
    (!taxes.is_empty()).then_some(taxes)
}

/// `(step_order, step_type, amount key, optional "days after paid thru" key)`.
/// Fixed to PS's actual template shape (1st/2nd/3rd late fee only) --
/// if a future PS template adds a 4th, this list needs a new entry.
const DELINQUENCY_AMOUNT_FIELDS: &[(i32, &str, &str, Option<&str>)] = &[
    (
        1,
        "late_fee",
        "1st_Late_Fee_Amount",
        Some("How_many_days_after_the_Paid_THRU_Date_is_the_1st_Late_Fee_applied?"),
    ),
    (
        2,
        "late_fee",
        "2nd_Late_Fee_Amount",
        Some("How_many_days_after_the_Paid_THRU_Date_is_the_2nd_Late_Fee_applied?"),
    ),
    (
        3,
        "late_fee",
        "3rd_Late_Fee_Amount",
        Some("How_many_days_after_the_Paid_THRU_Date_is_the_3rd_Late_Fee_applied?"),
    ),
    (
        4,
        "pre_lien",
        "Pre-Lien_Fee_Amount",
        Some("How_many_days_after_the_Paid_THRU_Date_is_the_Pre-Lien_Fee_applied?"),
    ),
    (
        5,
        "lien",
        "Lien_Fee_Amount",
        Some("How_many_days_after_the_Paid_THRU_Date_-OR-_the_Pre-Lien_is_the_Lien_Fee_applied?"),
    ),
    (6, "cut_lock", "Cut_Lock_Fee:", None),
    (7, "auction", "Auction_Advertising_Fee:", None),
];

fn map_delinquency_steps(fields: &[FormField]) -> Vec<MappedDelinquencyStep> {
    let mut steps = Vec::new();

    for (step_order, step_type, amount_key, days_key) in DELINQUENCY_AMOUNT_FIELDS {
        let Some(amount) = value_for(fields, amount_key) else {
            continue;
        };
        let raw_value = match days_key.and_then(|k| value_for(fields, k)) {
            Some(days) => format!("{amount} (charged {days} days after Paid Thru Date)"),
            None => amount,
        };
        steps.push(MappedDelinquencyStep {
            step_order: *step_order,
            step_type,
            raw_value,
        });
    }

    let notice_type = value_for(fields, "Which_type_of_late_notice_should_recur?");
    let lockout_days = value_for(fields, "How_many_days_after_Paid_THRU_Date_should_Lockout_occur?");
    if notice_type.is_some() || lockout_days.is_some() {
        let parts: Vec<String> = [
            notice_type,
            lockout_days.map(|d| format!("recurs after {d} days")),
        ]
        .into_iter()
        .flatten()
        .collect();
        steps.push(MappedDelinquencyStep {
            step_order: 8,
            step_type: "notice",
            raw_value: parts.join("; "),
        });
    }

    if let Some(raw_value) = value_for(fields, "Add'l_Delinquency_Actions:") {
        steps.push(MappedDelinquencyStep {
            step_order: 9,
            step_type: "other",
            raw_value,
        });
    }
    if let Some(raw_value) = value_for(fields, "Delinquency_Notes:") {
        steps.push(MappedDelinquencyStep {
            step_order: 10,
            step_type: "other",
            raw_value,
        });
    }

    steps
}

fn map_coverage_tiers(fields: &[FormField]) -> Vec<MappedCoverageTier> {
    let mut tiers = Vec::new();

    for tier_number in 1..=5 {
        let amount = value_for(
            fields,
            &format!("Coverage_Level_{tier_number}_-_Total_Coverage_Amount:"),
        );
        let cost = value_for(fields, &format!("Coverage_Level_{tier_number}_-_Cost_to_Tenant:"));
        if amount.is_some() || cost.is_some() {
            tiers.push(MappedCoverageTier {
                tier_number,
                total_coverage_amount_raw: amount,
                cost_to_tenant_raw: cost,
            });
        }
    }

    if let Some(extra) = value_for(fields, "Coverage_Level_6+") {
        tiers.push(MappedCoverageTier {
            tier_number: 6,
            total_coverage_amount_raw: Some(extra),
            cost_to_tenant_raw: None,
        });
    }

    tiers
}

fn map_commission(fields: &[FormField]) -> Option<MappedCommission> {
    let commission = MappedCommission {
        commission_type_raw: value_for(fields, "Is_Commission_a_Percentage_or_Flat_Dollar_Amount?"),
        dollar_amount_raw: value_for(fields, "$_Commission_Amount:"),
        percent_amount_raw: value_for(fields, "%_Commission_Amount:"),
    };
    (!commission.is_empty()).then_some(commission)
}

pub fn map_intake_fields(fields: &[FormField]) -> MappedIntakeRun {
    let company = MappedCompany {
        legal_name: value_for(fields, "What_is_the_name_of_your_Corporation_/_Business_Entity?")
            .or_else(|| value_for(fields, "Company_Name:")),
        corporate_email: value_for(fields, "What_is_your_Corporate_Email_Address?"),
        corporate_phone: value_for(fields, "What_is_your_Corporate_Phone_Number?"),
        corporate_address_street: value_for(fields, "Corporate_Street_Address:"),
        corporate_address_city: value_for(fields, "Corporate_City:"),
        corporate_address_state: value_for(fields, "Corporate_State:"),
        corporate_address_zip: value_for(fields, "Corporate_Zip:"),
        subdomain: value_for(fields, "Company_Subdomain:"),
        // MultiChoice field -- data.values (a JSON array), not
        // data.value, hence values_for rather than value_for.
        accepted_payment_methods: values_for(fields, "Accepted_Payment_Methods:"),
        accounting_basis: value_for(fields, "Accounting_Basis:"),
        payment_scheme: value_for(fields, "Payment_Scheme:"),
        offers_tenant_insurance_raw: value_for(
            fields,
            "Are_they_currently_offering_insurance/protection_to_their_tenants?",
        ),
        insurance_provider: value_for(fields, "Who_is_their_insurance_provider?"),
    };

    let facility = MappedFacility {
        name: value_for(fields, "Facility_Name"),
        street_address: value_for(fields, "Facility_Street_Address:"),
        city: value_for(fields, "Facility_City:"),
        state: value_for(fields, "Facility_State:"),
        zip: value_for(fields, "Facility_Zip:"),
        phone: value_for(fields, "What_is_the_facility_phone_number?"),
        email: value_for(fields, "What_is_the_facility_email_address?"),
        units_count: parse_units_count(fields, "How_many_units_does_this_facility_have?"),
        primary_storage_offering: value_for(fields, "What_is_the_PRIMARY_storage_offering_at_this_facility?"),
        previous_pms: value_for(
            fields,
            "What_Property_Management_Software_is_this_facility_currently_using?",
        ),
        access_control_system: value_for(fields, "What_Access_Control_system_are_they_using?"),
        go_live_date: parse_ps_date(fields, "What_is_the_Go_Live_Date_on_the_contract?"),
        dropbox_folder_url: value_for(fields, "Facility_Onboarding_folder_URL:"),
        subdomain: value_for(fields, "Facility_Subdomain:"),
        subdomain_exists_in_qms_raw: value_for(fields, "Does_the_Facility_Subdomain_already_exist_in_QMS?"),
        system_email: value_for(fields, "Facility_Email_Address:"),
    };

    MappedIntakeRun {
        is_first_time: parse_is_first_time(fields),
        company,
        facility,
        fees: map_fees(fields),
        taxes: map_taxes(fields),
        delinquency_steps: map_delinquency_steps(fields),
        coverage_tiers: map_coverage_tiers(fields),
        commission: map_commission(fields),
        specials_raw_text: value_for(fields, "Specials:"),
        owners: value_for(fields, "Owner_Level_Users:")
            .map(|raw| parse_people_block(&raw))
            .unwrap_or_default(),
        district_managers: value_for(fields, "District_Manager_Level_Users:")
            .map(|raw| parse_people_block(&raw))
            .unwrap_or_default(),
        managers: value_for(fields, "Manager_Level_Users")
            .map(|raw| parse_people_block(&raw))
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real Highway 20 Intake/Progress data captured this session --
    // nothing in this workflow is sensitive (no SSN/financial/
    // government-ID data lives here), so it's committed verbatim.
    const HIGHWAY20_INTAKE_FIELDS: &str = include_str!("testdata/highway20_intake_fields.json");

    fn real_fields() -> Vec<FormField> {
        serde_json::from_str(HIGHWAY20_INTAKE_FIELDS).expect("fixture must parse as Vec<FormField>")
    }

    #[test]
    fn maps_company_and_facility_from_the_real_highway20_run() {
        let mapped = map_intake_fields(&real_fields());

        assert_eq!(mapped.company.legal_name.as_deref(), Some("Prairie Enterprises LLC"));
        assert_eq!(mapped.facility.name.as_deref(), Some("Highway 20 Self Storage"));
        // Real value has trailing whitespace in PS's own export -- must come back trimmed.
        assert_eq!(mapped.facility.city.as_deref(), Some("Marengo"));
        assert_eq!(mapped.facility.units_count, Some(788));
    }

    #[test]
    fn maps_is_first_time_from_the_real_highway20_run() {
        // Highway 20 answered "Yes" -- see the vault's sister-site
        // writeup; it's the one real facility with a fully-answered
        // Corporate Info section in this fixture.
        let mapped = map_intake_fields(&real_fields());
        assert_eq!(mapped.is_first_time, Some(true));
    }

    #[test]
    fn is_first_time_is_none_when_the_field_is_unanswered() {
        let mapped = map_intake_fields(&[]);
        assert_eq!(mapped.is_first_time, None);
    }

    #[test]
    fn maps_the_company_page_financial_information_fields() {
        let mapped = map_intake_fields(&real_fields());

        assert!(mapped.company.accepted_payment_methods.is_some());
        assert!(mapped.company.accounting_basis.is_some());
        assert!(mapped.company.payment_scheme.is_some());
        assert!(mapped.company.offers_tenant_insurance_raw.is_some());
    }

    #[test]
    fn maps_the_qms_subdomain_and_system_email_setup_fields() {
        let mapped = map_intake_fields(&real_fields());

        assert_eq!(
            mapped.company.subdomain.as_deref(),
            Some("prairie-enterprises.qms-email.com")
        );
        assert_eq!(
            mapped.facility.subdomain.as_deref(),
            Some("tenant.highway20selfstorage.com")
        );
        assert_eq!(mapped.facility.subdomain_exists_in_qms_raw.as_deref(), Some("No"));
        assert_eq!(
            mapped.facility.system_email.as_deref(),
            Some("info@tenant.highway20selfstorage.com")
        );
        // Distinct from the general facility contact email -- both must
        // survive independently, not collapse into one field.
        assert_ne!(mapped.facility.system_email, mapped.facility.email);
    }

    #[test]
    fn maps_named_fees_and_keeps_any_other_fees_as_one_verbatim_blob() {
        let mapped = map_intake_fields(&real_fields());

        let other = mapped
            .fees
            .iter()
            .find(|f| f.fee_type == "other")
            .expect("Any Other Fees must map to one 'other' row");
        assert!(other.raw_value.contains("Optional Electrical"));
        assert!(
            other.raw_value.contains("Moving Truck - Weekday Use"),
            "the whole blob must stay together, not split into per-line rows"
        );
    }

    #[test]
    fn maps_delinquency_late_fee_steps_combining_amount_and_day_count() {
        let mapped = map_intake_fields(&real_fields());
        let first = mapped
            .delinquency_steps
            .iter()
            .find(|s| s.step_order == 1)
            .expect("1st late fee must be present on this real run");
        assert_eq!(first.step_type, "late_fee");
        assert!(first.raw_value.contains("$10.00"));
        assert!(first.raw_value.contains("7 days"));
    }

    #[test]
    fn maps_five_coverage_tiers_from_the_real_run() {
        let mapped = map_intake_fields(&real_fields());
        assert_eq!(mapped.coverage_tiers.len(), 5);
        assert_eq!(mapped.coverage_tiers[0].tier_number, 1);
        assert_eq!(
            mapped.coverage_tiers[0].total_coverage_amount_raw.as_deref(),
            Some("2000.00")
        );
    }

    #[test]
    fn parses_owner_and_manager_blocks_into_people() {
        let mapped = map_intake_fields(&real_fields());
        // Highway 20's real Owner Level Users block lists 3 people
        // (Kyle Lindley, Juanita Fleener, Judy Armstrong) in the
        // multi-line, blank-line-separated format -- see
        // people::parse_people_block's own tests for that format.
        assert_eq!(mapped.owners.len(), 3);
        assert_eq!(mapped.owners[0].full_name, "Kyle Lindley");
        assert!(!mapped.managers.is_empty() || !mapped.district_managers.is_empty());
    }

    #[test]
    fn taxes_and_commission_are_none_when_nothing_was_answered() {
        let empty: Vec<FormField> = Vec::new();
        let mapped = map_intake_fields(&empty);
        assert_eq!(mapped.taxes, None);
        assert_eq!(mapped.commission, None);
        assert!(mapped.fees.is_empty());
        assert!(mapped.delinquency_steps.is_empty());
        assert!(mapped.coverage_tiers.is_empty());
    }
}
