//! `GET /clients/{id}` -- assembles the company page from the per-query reads.

use super::dto::{CompanyDetailResponse, OwnerInfo};
use super::queries::{
    fetch_clickup_parent_history, fetch_company_facilities, fetch_company_row, fetch_elavon_active,
    fetch_owner_parties,
};
use crate::api::{internal_error, not_found, AppState};
use crate::auth::AuthenticatedUser;
use crate::clients::merchant_account_mapping::decrypt_party_pii;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use uuid::Uuid;

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
    let (company_result, facilities_result, elavon_result, owners_result, history_result) = tokio::join!(
        fetch_company_row(&state.db, user.user_id, &user.role_keys, company_id),
        fetch_company_facilities(&state.db, user.user_id, &user.role_keys, company_id),
        fetch_elavon_active(&state.db, user.user_id, &user.role_keys, company_id),
        fetch_owner_parties(&state.db, user.user_id, &user.role_keys, company_id),
        fetch_clickup_parent_history(&state.db, user.user_id, &user.role_keys, company_id),
    );

    let company = match company_result {
        Ok(company) => company,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "company detail query failed");
            return internal_error("Could not load this company");
        }
    };
    let Some(company) = company else {
        return not_found("not_found", "No such company.".to_string());
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

    let clickup_parent_history = match history_result {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "ClickUp parent history query failed");
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
                party_role: if row.party_role == "signer" {
                    "signer"
                } else {
                    "owner"
                },
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
        implementation_completed_at: company.implementation_completed_at,
        clickup_parent_facility_id: company.clickup_parent_facility_id,
        clickup_waived_at: company.clickup_waived_at,
        clickup_parent_history,
        elavon_active,
        facilities,
        owners,
    })
    .into_response()
}
