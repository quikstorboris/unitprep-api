//! The database row shapes the re-sync compares against Process Street.

use crate::clients::intake_mapping::{MappedCompany, MappedFacility};
use uuid::Uuid;

#[derive(sqlx::FromRow)]
pub(super) struct CompanyRow {
    pub(super) id: Uuid,
    pub(super) ps_intake_run_id: Option<String>,
    pub(super) legal_name: String,
    pub(super) corporate_email: Option<String>,
    pub(super) corporate_phone: Option<String>,
    pub(super) corporate_address_street: Option<String>,
    pub(super) corporate_address_city: Option<String>,
    pub(super) corporate_address_state: Option<String>,
    pub(super) corporate_address_zip: Option<String>,
    pub(super) subdomain: Option<String>,
    pub(super) accepted_payment_methods: Option<String>,
    pub(super) accounting_basis: Option<String>,
    pub(super) payment_scheme: Option<String>,
    pub(super) offers_tenant_insurance_raw: Option<String>,
    pub(super) insurance_provider: Option<String>,
    pub(super) website_url: Option<String>,
    pub(super) manually_edited_fields: Vec<String>,
}

impl CompanyRow {
    pub(super) fn mapped(&self) -> MappedCompany {
        MappedCompany {
            legal_name: Some(self.legal_name.clone()),
            corporate_email: self.corporate_email.clone(),
            corporate_phone: self.corporate_phone.clone(),
            corporate_address_street: self.corporate_address_street.clone(),
            corporate_address_city: self.corporate_address_city.clone(),
            corporate_address_state: self.corporate_address_state.clone(),
            corporate_address_zip: self.corporate_address_zip.clone(),
            subdomain: self.subdomain.clone(),
            accepted_payment_methods: self.accepted_payment_methods.clone(),
            accounting_basis: self.accounting_basis.clone(),
            payment_scheme: self.payment_scheme.clone(),
            offers_tenant_insurance_raw: self.offers_tenant_insurance_raw.clone(),
            insurance_provider: self.insurance_provider.clone(),
            website_url: self.website_url.clone(),
        }
    }
}

#[derive(sqlx::FromRow)]
pub(super) struct FacilityRow {
    pub(super) id: Uuid,
    pub(super) ps_intake_run_id: Option<String>,
    pub(super) name: String,
    pub(super) street_address: Option<String>,
    pub(super) city: Option<String>,
    pub(super) state: Option<String>,
    pub(super) zip: Option<String>,
    pub(super) phone: Option<String>,
    pub(super) email: Option<String>,
    pub(super) units_count: Option<i32>,
    pub(super) primary_storage_offering: Option<String>,
    pub(super) previous_pms: Option<String>,
    pub(super) access_control_system: Option<String>,
    pub(super) go_live_date: Option<chrono::NaiveDate>,
    pub(super) dropbox_folder_url: Option<String>,
    pub(super) subdomain: Option<String>,
    pub(super) subdomain_exists_in_qms_raw: Option<String>,
    pub(super) system_email: Option<String>,
    pub(super) website_url: Option<String>,
    pub(super) manually_edited_fields: Vec<String>,
}

impl FacilityRow {
    pub(super) fn mapped(&self) -> MappedFacility {
        MappedFacility {
            name: Some(self.name.clone()),
            street_address: self.street_address.clone(),
            city: self.city.clone(),
            state: self.state.clone(),
            zip: self.zip.clone(),
            phone: self.phone.clone(),
            email: self.email.clone(),
            units_count: self.units_count,
            primary_storage_offering: self.primary_storage_offering.clone(),
            previous_pms: self.previous_pms.clone(),
            access_control_system: self.access_control_system.clone(),
            go_live_date: self.go_live_date,
            dropbox_folder_url: self.dropbox_folder_url.clone(),
            subdomain: self.subdomain.clone(),
            subdomain_exists_in_qms_raw: self.subdomain_exists_in_qms_raw.clone(),
            system_email: self.system_email.clone(),
            website_url: self.website_url.clone(),
        }
    }
}
