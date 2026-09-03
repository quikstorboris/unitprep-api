//! Read endpoints behind Phase 4's Client record UI -- the Company page
//! (sections 1-3 of the vault's own design note: Company Information,
//! Financial Information, Owner(s) Information, plus the facility-
//! selector rail) and a facility's own General tab + Facility Policies
//! tab. Read-only, same "any authenticated caller" gate `clients_search`/
//! `clients_preview` already use -- the genuinely sensitive parts
//! (Elavon activity, owner PII) are protected by RLS itself
//! (`facility_merchant_accounts`/`facility_merchant_account_parties` are
//! `onboarding_manager`/`department_manager`-only at the database level),
//! so a caller without that role simply gets those fields back empty
//! rather than needing a second permission check duplicated here.
//!
//! **Scoped to display only, this pass** -- no update/edit endpoints
//! yet. The vault's "global edit convention" (per-section Edit button,
//! everything editable except Elavon credentials) is real future work,
//! sequenced after read access exists to build against.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clients::merchant_account_mapping::decrypt_party_pii;

fn not_found(entity: &'static str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ApiErrorBody { error: "not_found", message: format!("No such {entity}.") }),
    )
        .into_response()
}

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

#[derive(sqlx::FromRow)]
struct CompanyDetailRow {
    id: Uuid,
    legal_name: String,
    corporate_email: Option<String>,
    corporate_phone: Option<String>,
    corporate_address_street: Option<String>,
    corporate_address_city: Option<String>,
    corporate_address_state: Option<String>,
    corporate_address_zip: Option<String>,
    subdomain: Option<String>,
    accepted_payment_methods: Option<String>,
    accounting_basis: Option<String>,
    payment_scheme: Option<String>,
    offers_tenant_insurance_raw: Option<String>,
    insurance_provider: Option<String>,
    website_url: Option<String>,
    archived_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
struct OwnerPartyRow {
    facility_id: Uuid,
    facility_name: String,
    party_role: String,
    party_index: i32,
    display_name: Option<String>,
    title: Option<String>,
    ownership_percent: Option<f64>,
    email: Option<String>,
    phone: Option<String>,
    encrypted_pii: Option<Vec<u8>>,
}

async fn fetch_company_row(
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
         insurance_provider, website_url, archived_at \
         FROM clients.companies WHERE id = $1",
    )
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

async fn fetch_company_facilities(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    company_id: Uuid,
) -> Result<Vec<FacilitySummary>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let rows = sqlx::query_as(
        "SELECT id, name, dropbox_folder_url FROM clients.facilities WHERE company_id = $1 ORDER BY name",
    )
    .bind(company_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

async fn fetch_elavon_active(
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

async fn fetch_owner_parties(
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

/// Any authenticated caller -- see this module's own doc comment on why
/// the sensitive sections don't need a separate permission check here.
///
/// **The 4 independent reads below run concurrently, each on its own
/// short-lived RLS transaction** (2026-09-03 fix) -- originally 4
/// sequential round trips against the real Neon database (a genuinely
/// remote Postgres, not local), visible as a real load delay switching
/// between the Company page and a facility. None of these queries
/// depends on another's result (only on `company_id`, checked once
/// after all four return), so there is no correctness reason for them
/// to wait on each other -- only historical accident (the original code
/// happened to reuse one transaction, `clients::create`'s own
/// combined-fetch doc comment covers the same lesson for a different
/// endpoint).
pub async fn get_company_detail(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
) -> Response {
    let (company_result, facilities_result, elavon_result, owners_result) = tokio::join!(
        fetch_company_row(&state.db, user.user_id, &user.role_keys, company_id),
        fetch_company_facilities(&state.db, user.user_id, &user.role_keys, company_id),
        fetch_elavon_active(&state.db, user.user_id, &user.role_keys, company_id),
        fetch_owner_parties(&state.db, user.user_id, &user.role_keys, company_id),
    );

    let company = match company_result {
        Ok(company) => company,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "company detail query failed");
            return internal_error("Could not load this company");
        }
    };
    let Some(company) = company else {
        return not_found("company");
    };

    let facilities = match facilities_result {
        Ok(facilities) => facilities,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "company facilities query failed");
            return internal_error("Could not load this company");
        }
    };

    let elavon_active = match elavon_result {
        Ok(active) => active,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "elavon-active query failed");
            return internal_error("Could not load this company");
        }
    };

    let owner_parties = match owners_result {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "owner parties query failed");
            return internal_error("Could not load this company");
        }
    };

    let owners = owner_parties
        .into_iter()
        .map(|row| {
            let pii = row.encrypted_pii.as_deref().and_then(|blob| {
                match decrypt_party_pii(row.facility_id, &row.party_role, row.party_index, blob) {
                    Ok(pii) => Some(pii),
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            facility_id = %row.facility_id,
                            party_role = %row.party_role,
                            party_index = row.party_index,
                            "failed to decrypt a party's PII for the Owner(s) Information section"
                        );
                        None
                    }
                }
            });

            OwnerInfo {
                facility_id: row.facility_id,
                facility_name: row.facility_name,
                party_role: if row.party_role == "signer" { "signer" } else { "owner" },
                display_name: row.display_name,
                title: row.title,
                ownership_percent: row.ownership_percent,
                email: row.email,
                phone: row.phone,
                ssn: pii.as_ref().and_then(|p| p.ssn.clone()),
                dob: pii.as_ref().and_then(|p| p.dob.clone()),
                home_address_line1: pii.as_ref().and_then(|p| p.home_address_line1.clone()),
                home_city: pii.as_ref().and_then(|p| p.home_city.clone()),
                home_state_or_province: pii.as_ref().and_then(|p| p.home_state_or_province.clone()),
                home_postal_code: pii.as_ref().and_then(|p| p.home_postal_code.clone()),
            }
        })
        .collect();

    Json(CompanyDetailResponse {
        id: company.id,
        legal_name: company.legal_name,
        corporate_email: company.corporate_email,
        corporate_phone: company.corporate_phone,
        corporate_address_street: company.corporate_address_street,
        corporate_address_city: company.corporate_address_city,
        corporate_address_state: company.corporate_address_state,
        corporate_address_zip: company.corporate_address_zip,
        subdomain: company.subdomain,
        accepted_payment_methods: company.accepted_payment_methods,
        accounting_basis: company.accounting_basis,
        payment_scheme: company.payment_scheme,
        offers_tenant_insurance_raw: company.offers_tenant_insurance_raw,
        insurance_provider: company.insurance_provider,
        website_url: company.website_url,
        archived_at: company.archived_at,
        elavon_active,
        facilities,
        owners,
    })
    .into_response()
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct FacilityDetailResponse {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
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
    pub subdomain: Option<String>,
    pub subdomain_exists_in_qms_raw: Option<String>,
    pub system_email: Option<String>,
    pub website_url: Option<String>,
}

/// Any authenticated caller -- General tab is plain facility contact
/// info, nothing sensitive.
pub async fn get_facility_detail(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for facility detail");
            return internal_error("Could not load this facility");
        }
    };

    let facility: Option<FacilityDetailResponse> = match sqlx::query_as(
        "SELECT id, company_id, name, street_address, city, state, zip, phone, email, \
         units_count, primary_storage_offering, previous_pms, access_control_system, \
         go_live_date, dropbox_folder_url, subdomain, subdomain_exists_in_qms_raw, system_email, \
         website_url \
         FROM clients.facilities WHERE id = $1 AND company_id = $2",
    )
    .bind(facility_id)
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(facility) => facility,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility detail query failed");
            return internal_error("Could not load this facility");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit facility detail transaction");
        return internal_error("Could not load this facility");
    }

    match facility {
        Some(facility) => Json(facility).into_response(),
        None => not_found("facility"),
    }
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct FeeRow {
    pub fee_type: String,
    pub label: Option<String>,
    pub raw_value: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TaxesRow {
    pub sales_tax_applies_raw: Option<String>,
    pub sales_tax_rate_raw: Option<String>,
    pub rent_tax_applies_raw: Option<String>,
    pub rent_tax_rate_raw: Option<String>,
    pub rent_tax_applies_to_all_units_raw: Option<String>,
    pub other_one_time_taxes_raw: Option<String>,
    pub other_recurring_taxes_raw: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct DelinquencyStepRow {
    pub step_order: i32,
    pub step_type: String,
    pub raw_value: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CoverageTierRow {
    pub tier_number: i32,
    pub total_coverage_amount_raw: Option<String>,
    pub cost_to_tenant_raw: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CommissionRow {
    pub commission_type_raw: Option<String>,
    pub dollar_amount_raw: Option<String>,
    pub percent_amount_raw: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FacilityPoliciesResponse {
    pub fees: Vec<FeeRow>,
    pub taxes: Option<TaxesRow>,
    pub delinquency_steps: Vec<DelinquencyStepRow>,
    pub coverage_tiers: Vec<CoverageTierRow>,
    pub commission: Option<CommissionRow>,
    pub specials_raw_text: Option<String>,
}

async fn fetch_facility_exists(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    company_id: Uuid,
    facility_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM clients.facilities WHERE id = $1 AND company_id = $2")
        .bind(facility_id)
        .bind(company_id)
        .fetch_optional(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(row.is_some())
}

async fn fetch_policy_fees(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Vec<FeeRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let rows = sqlx::query_as(
        "SELECT fee_type, label, raw_value FROM clients.policy_fees \
         WHERE facility_policies_id = $1 ORDER BY id",
    )
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

async fn fetch_policy_taxes(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Option<TaxesRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let row = sqlx::query_as(
        "SELECT sales_tax_applies_raw, sales_tax_rate_raw, rent_tax_applies_raw, rent_tax_rate_raw, \
         rent_tax_applies_to_all_units_raw, other_one_time_taxes_raw, other_recurring_taxes_raw \
         FROM clients.policy_taxes WHERE facility_policies_id = $1",
    )
    .bind(facility_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

async fn fetch_delinquency_steps(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Vec<DelinquencyStepRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let rows = sqlx::query_as(
        "SELECT step_order, step_type, raw_value FROM clients.policy_delinquency_steps \
         WHERE facility_policies_id = $1 ORDER BY step_order",
    )
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

async fn fetch_coverage_tiers(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Vec<CoverageTierRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let rows = sqlx::query_as(
        "SELECT tier_number, total_coverage_amount_raw, cost_to_tenant_raw \
         FROM clients.policy_coverage_tiers WHERE facility_policies_id = $1 ORDER BY tier_number",
    )
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

async fn fetch_commission(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Option<CommissionRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let row = sqlx::query_as(
        "SELECT commission_type_raw, dollar_amount_raw, percent_amount_raw \
         FROM clients.policy_commission WHERE facility_policies_id = $1",
    )
    .bind(facility_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

async fn fetch_specials_raw_text(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT raw_text FROM clients.policy_specials WHERE facility_policies_id = $1")
            .bind(facility_id)
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(row.and_then(|(text,)| text))
}

/// Any authenticated caller -- Facility Policies carries no PII (fees,
/// taxes, delinquency steps, coverage, commission, specials are all
/// business terms, not personal data).
///
/// **The 7 reads below run concurrently** (2026-09-03 fix, same
/// rationale as `get_company_detail`'s own doc comment). The existence
/// check no longer gates the other 6 queries -- it only gates whether
/// their results get used, since a nonexistent `facility_id` simply
/// makes every other query return empty/None anyway, which is thrown
/// away on the 404 path regardless.
pub async fn get_facility_policies(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let (exists_result, fees_result, taxes_result, steps_result, tiers_result, commission_result, specials_result) = tokio::join!(
        fetch_facility_exists(&state.db, user.user_id, &user.role_keys, company_id, facility_id),
        fetch_policy_fees(&state.db, user.user_id, &user.role_keys, facility_id),
        fetch_policy_taxes(&state.db, user.user_id, &user.role_keys, facility_id),
        fetch_delinquency_steps(&state.db, user.user_id, &user.role_keys, facility_id),
        fetch_coverage_tiers(&state.db, user.user_id, &user.role_keys, facility_id),
        fetch_commission(&state.db, user.user_id, &user.role_keys, facility_id),
        fetch_specials_raw_text(&state.db, user.user_id, &user.role_keys, facility_id),
    );

    // Confirms the facility exists and belongs to this company. A
    // facility with no facility_policies row at all (never ingested, or
    // a manual facility) still returns a real 200 with every section
    // empty, not a 404, since "no policies captured yet" is a
    // legitimate state, not a missing-resource error.
    let exists = match exists_result {
        Ok(exists) => exists,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility existence check failed");
            return internal_error("Could not load this facility's policies");
        }
    };
    if !exists {
        return not_found("facility");
    }

    let fees = match fees_result {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "policy_fees query failed");
            return internal_error("Could not load this facility's policies");
        }
    };

    let taxes = match taxes_result {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "policy_taxes query failed");
            return internal_error("Could not load this facility's policies");
        }
    };

    let delinquency_steps = match steps_result {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "policy_delinquency_steps query failed");
            return internal_error("Could not load this facility's policies");
        }
    };

    let coverage_tiers = match tiers_result {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "policy_coverage_tiers query failed");
            return internal_error("Could not load this facility's policies");
        }
    };

    let commission = match commission_result {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "policy_commission query failed");
            return internal_error("Could not load this facility's policies");
        }
    };

    let specials_raw_text = match specials_result {
        Ok(text) => text,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "policy_specials query failed");
            return internal_error("Could not load this facility's policies");
        }
    };

    Json(FacilityPoliciesResponse { fees, taxes, delinquency_steps, coverage_tiers, commission, specials_raw_text })
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn get_company_detail_returns_404_for_the_unreachable_test_pool_as_a_500() {
        // Same convention as every other handler test in this codebase:
        // empty_state()'s pool never connects, so any handler that
        // reaches the database at all surfaces as a 500 -- the success
        // signal here is "it reached the query", not a real 404.
        let response = get_company_detail(State(empty_state()), test_user(), Path(Uuid::new_v4())).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn get_facility_detail_reaches_the_database() {
        let response =
            get_facility_detail(State(empty_state()), test_user(), Path((Uuid::new_v4(), Uuid::new_v4()))).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn get_facility_policies_reaches_the_database() {
        let response =
            get_facility_policies(State(empty_state()), test_user(), Path((Uuid::new_v4(), Uuid::new_v4()))).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
