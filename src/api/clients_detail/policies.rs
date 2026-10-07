//! `GET .../policies` -- assembles a facility's policies from the per-table queries.

use super::policy_dto::FacilityPoliciesResponse;
use super::policy_queries::{
    fetch_commission, fetch_coverage_tiers, fetch_delinquency_entries, fetch_delinquency_steps,
    fetch_facility_exists, fetch_policy_fees, fetch_policy_flags_and_qsx_status,
    fetch_policy_taxes, fetch_specials_raw_text, fetch_tax_entries,
};
use crate::api::{internal_error, not_found, AppState};
use crate::auth::AuthenticatedUser;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use uuid::Uuid;

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
    let (
        exists_result,
        fees_result,
        taxes_result,
        tax_entries_result,
        steps_result,
        delinquency_entries_result,
        tiers_result,
        commission_result,
        specials_result,
        flags_result,
    ) = tokio::join!(
        fetch_facility_exists(
            &state.db,
            user.user_id,
            &user.role_keys,
            company_id,
            facility_id
        ),
        fetch_policy_fees(&state.db, user.user_id, &user.role_keys, facility_id),
        fetch_policy_taxes(&state.db, user.user_id, &user.role_keys, facility_id),
        fetch_tax_entries(&state.db, user.user_id, &user.role_keys, facility_id),
        fetch_delinquency_steps(&state.db, user.user_id, &user.role_keys, facility_id),
        fetch_delinquency_entries(&state.db, user.user_id, &user.role_keys, facility_id),
        fetch_coverage_tiers(&state.db, user.user_id, &user.role_keys, facility_id),
        fetch_commission(&state.db, user.user_id, &user.role_keys, facility_id),
        fetch_specials_raw_text(&state.db, user.user_id, &user.role_keys, facility_id),
        fetch_policy_flags_and_qsx_status(&state.db, user.user_id, &user.role_keys, facility_id),
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
        return not_found("not_found", "No such facility.".to_string());
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

    let tax_entries = match tax_entries_result {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "policy_tax_entries query failed");
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

    let delinquency_entries = match delinquency_entries_result {
        Ok(rows) => rows,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "policy_delinquency_entries query failed");
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

    let (flags, is_qsx_legacy) = match flags_result {
        Ok(result) => result,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility_policies flags query failed");
            return internal_error("Could not load this facility's policies");
        }
    };

    Json(FacilityPoliciesResponse {
        fees,
        taxes,
        tax_entries,
        delinquency_steps,
        delinquency_entries,
        coverage_tiers,
        commission,
        specials_raw_text,
        is_qsx_legacy,
        fees_manually_exempt: flags.fees_manually_exempt,
        taxes_manually_exempt: flags.taxes_manually_exempt,
        delinquency_manually_exempt: flags.delinquency_manually_exempt,
        coverage_manually_exempt: flags.coverage_manually_exempt,
        specials_manually_exempt: flags.specials_manually_exempt,
    })
    .into_response()
}
