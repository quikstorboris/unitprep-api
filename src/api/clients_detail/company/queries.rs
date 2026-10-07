//! The read queries behind the company page: the company row, its facilities, Elavon status and owner parties.

use super::dto::FacilitySummary;
use crate::auth::begin_rls_transaction;
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(sqlx::FromRow)]
pub(super) struct CompanyDetailRow {
    pub(super) id: Uuid,
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
    pub(super) archived_at: Option<DateTime<Utc>>,
    pub(super) implementation_completed_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
pub(super) struct OwnerPartyRow {
    pub(super) facility_id: Uuid,
    pub(super) facility_name: String,
    pub(super) party_role: String,
    pub(super) party_index: i32,
    pub(super) display_name: Option<String>,
    pub(super) title: Option<String>,
    pub(super) ownership_percent: Option<f64>,
    pub(super) email: Option<String>,
    pub(super) phone: Option<String>,
    pub(super) encrypted_pii: Option<Vec<u8>>,
}

pub(super) async fn fetch_company_row(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    company_id: Uuid,
) -> Result<Option<CompanyDetailRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let row = sqlx::query_as(
        "SELECT id, legal_name, corporate_email, corporate_phone, corporate_address_street, \
         corporate_address_city, corporate_address_state, corporate_address_zip, subdomain, \
         accepted_payment_methods, accounting_basis, payment_scheme, offers_tenant_insurance_raw, \
         insurance_provider, website_url, archived_at, implementation_completed_at \
         FROM clients.companies WHERE id = $1",
    )
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

pub(super) async fn fetch_company_facilities(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    company_id: Uuid,
) -> Result<Vec<FacilitySummary>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let rows = sqlx::query_as(
        "SELECT id, name, dropbox_folder_url, clickup_list_id, clickup_list_name, \
         clickup_folder_name, clickup_list_url \
         FROM clients.facilities WHERE company_id = $1 ORDER BY name",
    )
    .bind(company_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

pub(super) async fn fetch_elavon_active(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    company_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let active = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM clients.facility_merchant_accounts fma \
         JOIN clients.facilities f ON f.id = fma.facility_id WHERE f.company_id = $1)",
    )
    .bind(company_id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(active)
}

pub(super) async fn fetch_owner_parties(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    company_id: Uuid,
) -> Result<Vec<OwnerPartyRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    // Owner(s) Information -- only owner/signer parties (never
    // intermediary_business, which has no PII), decrypted per row by the
    // caller. A party whose decryption fails (wrong/missing key,
    // corrupted blob) is skipped with its other fields still shown --
    // degrades that one row's PII, not the whole page.
    // ownership_percent is NUMERIC in Postgres -- sqlx has no built-in
    // decode from NUMERIC to plain f64 (it wants rust_decimal/bigdecimal,
    // neither of which this crate depends on), so it's cast to float8 in
    // SQL instead. Never caught before 2026-09-03 because no facility
    // had a real party row until the Elavon tab's first live link.
    let rows = sqlx::query_as(
        "SELECT p.facility_id, f.name AS facility_name, p.party_role, p.party_index, \
         p.display_name, p.title, p.ownership_percent::float8 AS ownership_percent, p.email, p.phone, \
         p.encrypted_pii \
         FROM clients.facility_merchant_account_parties p \
         JOIN clients.facilities f ON f.id = p.facility_id \
         WHERE f.company_id = $1 AND p.party_role IN ('owner', 'signer') \
         ORDER BY f.name, p.party_index",
    )
    .bind(company_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}
