//! Response shape for the company page.

use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct FacilitySummary {
    pub id: Uuid,
    pub name: String,
    /// Carried onto the Company page's own Dropbox section (2026-09-03) --
    /// there's no company-level Dropbox field in the schema (Intake only
    /// ever captures it per facility), so "Dropbox on the Company page"
    /// is a list of each facility's own link, same pattern as Owner(s)
    /// Information below.
    pub dropbox_folder_url: Option<String>,
    /// The facility's ClickUp onboarding list, if linked (see
    /// `clients_clickup_links`). Name/folder/URL are a snapshot taken at
    /// link time; only the list id is authoritative.
    pub clickup_list_id: Option<String>,
    pub clickup_list_name: Option<String>,
    pub clickup_folder_name: Option<String>,
    pub clickup_list_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OwnerInfo {
    pub facility_id: Uuid,
    pub facility_name: String,
    /// "owner" | "signer" -- intermediary_business parties are excluded,
    /// see this endpoint's own query (they have no PII to show here and
    /// aren't a "person" the Owner(s) section is meant to list).
    pub party_role: &'static str,
    pub display_name: Option<String>,
    pub title: Option<String>,
    pub ownership_percent: Option<f64>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub ssn: Option<String>,
    pub dob: Option<String>,
    pub home_address_line1: Option<String>,
    pub home_city: Option<String>,
    pub home_state_or_province: Option<String>,
    pub home_postal_code: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CompanyDetailResponse {
    pub id: Uuid,
    pub legal_name: String,
    pub corporate_email: Option<String>,
    pub corporate_phone: Option<String>,
    pub corporate_address_street: Option<String>,
    pub corporate_address_city: Option<String>,
    pub corporate_address_state: Option<String>,
    pub corporate_address_zip: Option<String>,
    pub subdomain: Option<String>,
    pub accepted_payment_methods: Option<String>,
    pub accounting_basis: Option<String>,
    pub payment_scheme: Option<String>,
    pub offers_tenant_insurance_raw: Option<String>,
    pub insurance_provider: Option<String>,
    pub website_url: Option<String>,
    pub archived_at: Option<DateTime<Utc>>,
    /// See `clients_implementation_status`; drives the company page's
    /// "Implementation Completed" toggle.
    pub implementation_completed_at: Option<DateTime<Utc>>,
    /// Whether any of this company's facilities has a Merchant Account
    /// record at all -- computed at read time, not stored (see the
    /// vault's own note: "no new schema needed... a read-time query
    /// across the company's facilities"). `false` for a caller whose
    /// role can't see `facility_merchant_accounts` under RLS, same as
    /// `owners` below silently coming back empty for the same caller --
    /// not a data leak, just this field degrading along with the table
    /// it's computed from.
    pub elavon_active: bool,
    pub facilities: Vec<FacilitySummary>,
    pub owners: Vec<OwnerInfo>,
}
