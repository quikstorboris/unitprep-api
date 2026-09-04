//! Manual editing for the split Fees/Taxes/Delinquency/Coverage/Specials
//! tabs -- the first editable data anywhere in this app. Each handler
//! replaces one category's data wholesale (simplest correct semantics
//! for a form save, matching how `api::clients_facility_people` already
//! treats "Add User" as an upsert rather than a patch) and then, via
//! `clients::policy_exemption::mark_exempt_if_qsx_and_was_empty`, flags
//! that category permanently exempt from any future policy-sync pass --
//! but only when it was genuinely empty before this write and the
//! facility is QSX-legacy. A category that already had real data (from
//! Process Street or a previous manual edit) never gets that exemption;
//! it stays subject to whatever conflict resolution a future
//! policy-sync extension of the "Re-sync" screen adds.
//!
//! No extra permission check beyond authentication -- RLS already gates
//! every one of these tables' INSERT/UPDATE/DELETE to
//! `onboarding_manager`/`department_manager` (see the schema migration's
//! own per-table policy loop), the same reasoning
//! `clients_facility_people`'s own module doc gives for skipping a
//! second, app-level check.

use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clients::policy_exemption::{mark_exempt_if_qsx_and_was_empty, PolicyCategory};

const FEE_TYPES: &[&str] = &["security_deposit", "nsf_chargeback", "move_in_admin", "transfer", "cleaning", "other"];
const STEP_TYPES: &[&str] = &["late_fee", "pre_lien", "lien", "cut_lock", "auction", "notice", "other"];

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(ApiErrorBody { error: "not_found", message: "No such facility.".to_string() }))
        .into_response()
}

fn bad_request(message: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(ApiErrorBody { error: "invalid_request", message })).into_response()
}

async fn ensure_facility_and_policies_row(
    tx: &mut Transaction<'_, Postgres>,
    company_id: Uuid,
    facility_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM clients.facilities WHERE id = $1 AND company_id = $2")
            .bind(facility_id)
            .bind(company_id)
            .fetch_optional(&mut **tx)
            .await?;
    if exists.is_none() {
        return Ok(false);
    }

    // A facility that never had any Facility Policies data at all (a
    // manual facility, or one ingested before this row existed) has no
    // `facility_policies` row yet -- every category's child tables FK
    // reference it, so it must exist before any of them can.
    sqlx::query("INSERT INTO clients.facility_policies (facility_id) VALUES ($1) ON CONFLICT (facility_id) DO NOTHING")
        .bind(facility_id)
        .execute(&mut **tx)
        .await?;

    Ok(true)
}

#[derive(Debug, Deserialize)]
pub struct FeeInput {
    pub fee_type: String,
    pub label: Option<String>,
    pub raw_value: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateFeesRequest {
    pub fees: Vec<FeeInput>,
}

pub async fn update_fees(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateFeesRequest>,
) -> Response {
    if let Some(fee) = request.fees.iter().find(|f| !FEE_TYPES.contains(&f.fee_type.as_str())) {
        return bad_request(format!("\"{}\" is not a recognized fee type.", fee.fee_type));
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for update_fees");
            return internal_error("Could not save fees");
        }
    };

    match ensure_facility_and_policies_row(&mut tx, company_id, facility_id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = tx.rollback().await;
            return not_found();
        }
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for update_fees failed");
            return internal_error("Could not save fees");
        }
    }

    let was_empty: (i64,) = match sqlx::query_as("SELECT count(*) FROM clients.policy_fees WHERE facility_policies_id = $1")
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
    {
        Ok(row) => row,
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "policy_fees count failed");
            return internal_error("Could not save fees");
        }
    };
    let was_empty = was_empty.0 == 0;

    if let Err(err) = sqlx::query("DELETE FROM clients.policy_fees WHERE facility_policies_id = $1")
        .bind(facility_id)
        .execute(&mut *tx)
        .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "policy_fees delete failed");
        return internal_error("Could not save fees");
    }

    for fee in &request.fees {
        if let Err(err) = sqlx::query(
            "INSERT INTO clients.policy_fees (facility_policies_id, fee_type, label, raw_value) VALUES ($1, $2, $3, $4)",
        )
        .bind(facility_id)
        .bind(&fee.fee_type)
        .bind(&fee.label)
        .bind(&fee.raw_value)
        .execute(&mut *tx)
        .await
        {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "policy_fees insert failed");
            return internal_error("Could not save fees");
        }
    }

    if let Err(err) = mark_exempt_if_qsx_and_was_empty(&mut tx, facility_id, PolicyCategory::Fees, was_empty).await {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "fees exemption update failed");
        return internal_error("Could not save fees");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit update_fees transaction");
        return internal_error("Could not save fees");
    }

    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaxesRequest {
    pub sales_tax_applies_raw: Option<String>,
    pub sales_tax_rate_raw: Option<String>,
    pub rent_tax_applies_raw: Option<String>,
    pub rent_tax_rate_raw: Option<String>,
    pub rent_tax_applies_to_all_units_raw: Option<String>,
    pub other_one_time_taxes_raw: Option<String>,
    pub other_recurring_taxes_raw: Option<String>,
}

pub async fn update_taxes(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateTaxesRequest>,
) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for update_taxes");
            return internal_error("Could not save taxes");
        }
    };

    match ensure_facility_and_policies_row(&mut tx, company_id, facility_id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = tx.rollback().await;
            return not_found();
        }
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for update_taxes failed");
            return internal_error("Could not save taxes");
        }
    }

    let existing: Option<(Uuid,)> =
        match sqlx::query_as("SELECT facility_policies_id FROM clients.policy_taxes WHERE facility_policies_id = $1")
            .bind(facility_id)
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(err) => {
                let _ = tx.rollback().await;
                tracing::error!(error = %err, user_id = %user.user_id, "policy_taxes existence check failed");
                return internal_error("Could not save taxes");
            }
        };
    let was_empty = existing.is_none();

    if let Err(err) = sqlx::query(
        "INSERT INTO clients.policy_taxes
            (facility_policies_id, sales_tax_applies_raw, sales_tax_rate_raw, rent_tax_applies_raw,
             rent_tax_rate_raw, rent_tax_applies_to_all_units_raw, other_one_time_taxes_raw,
             other_recurring_taxes_raw)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (facility_policies_id) DO UPDATE SET
             sales_tax_applies_raw = EXCLUDED.sales_tax_applies_raw,
             sales_tax_rate_raw = EXCLUDED.sales_tax_rate_raw,
             rent_tax_applies_raw = EXCLUDED.rent_tax_applies_raw,
             rent_tax_rate_raw = EXCLUDED.rent_tax_rate_raw,
             rent_tax_applies_to_all_units_raw = EXCLUDED.rent_tax_applies_to_all_units_raw,
             other_one_time_taxes_raw = EXCLUDED.other_one_time_taxes_raw,
             other_recurring_taxes_raw = EXCLUDED.other_recurring_taxes_raw",
    )
    .bind(facility_id)
    .bind(&request.sales_tax_applies_raw)
    .bind(&request.sales_tax_rate_raw)
    .bind(&request.rent_tax_applies_raw)
    .bind(&request.rent_tax_rate_raw)
    .bind(&request.rent_tax_applies_to_all_units_raw)
    .bind(&request.other_one_time_taxes_raw)
    .bind(&request.other_recurring_taxes_raw)
    .execute(&mut *tx)
    .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "policy_taxes upsert failed");
        return internal_error("Could not save taxes");
    }

    if let Err(err) = mark_exempt_if_qsx_and_was_empty(&mut tx, facility_id, PolicyCategory::Taxes, was_empty).await {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "taxes exemption update failed");
        return internal_error("Could not save taxes");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit update_taxes transaction");
        return internal_error("Could not save taxes");
    }

    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize)]
pub struct DelinquencyStepInput {
    pub step_order: i32,
    pub step_type: String,
    pub raw_value: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDelinquencyRequest {
    pub steps: Vec<DelinquencyStepInput>,
}

pub async fn update_delinquency(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateDelinquencyRequest>,
) -> Response {
    if let Some(step) = request.steps.iter().find(|s| !STEP_TYPES.contains(&s.step_type.as_str())) {
        return bad_request(format!("\"{}\" is not a recognized delinquency step type.", step.step_type));
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for update_delinquency");
            return internal_error("Could not save delinquency steps");
        }
    };

    match ensure_facility_and_policies_row(&mut tx, company_id, facility_id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = tx.rollback().await;
            return not_found();
        }
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for update_delinquency failed");
            return internal_error("Could not save delinquency steps");
        }
    }

    let was_empty: (i64,) =
        match sqlx::query_as("SELECT count(*) FROM clients.policy_delinquency_steps WHERE facility_policies_id = $1")
            .bind(facility_id)
            .fetch_one(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(err) => {
                let _ = tx.rollback().await;
                tracing::error!(error = %err, user_id = %user.user_id, "policy_delinquency_steps count failed");
                return internal_error("Could not save delinquency steps");
            }
        };
    let was_empty = was_empty.0 == 0;

    if let Err(err) = sqlx::query("DELETE FROM clients.policy_delinquency_steps WHERE facility_policies_id = $1")
        .bind(facility_id)
        .execute(&mut *tx)
        .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "policy_delinquency_steps delete failed");
        return internal_error("Could not save delinquency steps");
    }

    for step in &request.steps {
        if let Err(err) = sqlx::query(
            "INSERT INTO clients.policy_delinquency_steps (facility_policies_id, step_order, step_type, raw_value) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(facility_id)
        .bind(step.step_order)
        .bind(&step.step_type)
        .bind(&step.raw_value)
        .execute(&mut *tx)
        .await
        {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "policy_delinquency_steps insert failed");
            return internal_error("Could not save delinquency steps");
        }
    }

    if let Err(err) =
        mark_exempt_if_qsx_and_was_empty(&mut tx, facility_id, PolicyCategory::Delinquency, was_empty).await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "delinquency exemption update failed");
        return internal_error("Could not save delinquency steps");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit update_delinquency transaction");
        return internal_error("Could not save delinquency steps");
    }

    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize)]
pub struct CoverageTierInput {
    pub tier_number: i32,
    pub total_coverage_amount_raw: Option<String>,
    pub cost_to_tenant_raw: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CommissionInput {
    pub commission_type_raw: Option<String>,
    pub dollar_amount_raw: Option<String>,
    pub percent_amount_raw: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCoverageRequest {
    pub tiers: Vec<CoverageTierInput>,
    /// `None` clears any existing commission row entirely -- commission
    /// folds into this same Coverage tab (per the original design note:
    /// "earned off insurance/protection-plan sales, so it belongs with
    /// it"), not a separate category with its own exemption flag.
    pub commission: Option<CommissionInput>,
}

pub async fn update_coverage(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateCoverageRequest>,
) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for update_coverage");
            return internal_error("Could not save coverage");
        }
    };

    match ensure_facility_and_policies_row(&mut tx, company_id, facility_id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = tx.rollback().await;
            return not_found();
        }
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for update_coverage failed");
            return internal_error("Could not save coverage");
        }
    }

    let tiers_count: (i64,) =
        match sqlx::query_as("SELECT count(*) FROM clients.policy_coverage_tiers WHERE facility_policies_id = $1")
            .bind(facility_id)
            .fetch_one(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(err) => {
                let _ = tx.rollback().await;
                tracing::error!(error = %err, user_id = %user.user_id, "policy_coverage_tiers count failed");
                return internal_error("Could not save coverage");
            }
        };
    let commission_exists: Option<(Uuid,)> = match sqlx::query_as(
        "SELECT facility_policies_id FROM clients.policy_commission WHERE facility_policies_id = $1",
    )
    .bind(facility_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "policy_commission existence check failed");
            return internal_error("Could not save coverage");
        }
    };
    let was_empty = tiers_count.0 == 0 && commission_exists.is_none();

    if let Err(err) = sqlx::query("DELETE FROM clients.policy_coverage_tiers WHERE facility_policies_id = $1")
        .bind(facility_id)
        .execute(&mut *tx)
        .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "policy_coverage_tiers delete failed");
        return internal_error("Could not save coverage");
    }

    for tier in &request.tiers {
        if let Err(err) = sqlx::query(
            "INSERT INTO clients.policy_coverage_tiers \
             (facility_policies_id, tier_number, total_coverage_amount_raw, cost_to_tenant_raw) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(facility_id)
        .bind(tier.tier_number)
        .bind(&tier.total_coverage_amount_raw)
        .bind(&tier.cost_to_tenant_raw)
        .execute(&mut *tx)
        .await
        {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "policy_coverage_tiers insert failed");
            return internal_error("Could not save coverage");
        }
    }

    if let Err(err) = sqlx::query("DELETE FROM clients.policy_commission WHERE facility_policies_id = $1")
        .bind(facility_id)
        .execute(&mut *tx)
        .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "policy_commission delete failed");
        return internal_error("Could not save coverage");
    }

    if let Some(commission) = &request.commission {
        if let Err(err) = sqlx::query(
            "INSERT INTO clients.policy_commission \
             (facility_policies_id, commission_type_raw, dollar_amount_raw, percent_amount_raw) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(facility_id)
        .bind(&commission.commission_type_raw)
        .bind(&commission.dollar_amount_raw)
        .bind(&commission.percent_amount_raw)
        .execute(&mut *tx)
        .await
        {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "policy_commission insert failed");
            return internal_error("Could not save coverage");
        }
    }

    if let Err(err) = mark_exempt_if_qsx_and_was_empty(&mut tx, facility_id, PolicyCategory::Coverage, was_empty).await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "coverage exemption update failed");
        return internal_error("Could not save coverage");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit update_coverage transaction");
        return internal_error("Could not save coverage");
    }

    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize)]
pub struct UpdateSpecialsRequest {
    pub raw_text: Option<String>,
}

pub async fn update_specials(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateSpecialsRequest>,
) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for update_specials");
            return internal_error("Could not save specials");
        }
    };

    match ensure_facility_and_policies_row(&mut tx, company_id, facility_id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = tx.rollback().await;
            return not_found();
        }
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for update_specials failed");
            return internal_error("Could not save specials");
        }
    }

    let existing: Option<(Uuid,)> = match sqlx::query_as(
        "SELECT facility_policies_id FROM clients.policy_specials WHERE facility_policies_id = $1",
    )
    .bind(facility_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "policy_specials existence check failed");
            return internal_error("Could not save specials");
        }
    };
    let was_empty = existing.is_none();

    if let Err(err) = sqlx::query(
        "INSERT INTO clients.policy_specials (facility_policies_id, raw_text) VALUES ($1, $2)
         ON CONFLICT (facility_policies_id) DO UPDATE SET raw_text = EXCLUDED.raw_text",
    )
    .bind(facility_id)
    .bind(&request.raw_text)
    .execute(&mut *tx)
    .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "policy_specials upsert failed");
        return internal_error("Could not save specials");
    }

    if let Err(err) = mark_exempt_if_qsx_and_was_empty(&mut tx, facility_id, PolicyCategory::Specials, was_empty).await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "specials exemption update failed");
        return internal_error("Could not save specials");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit update_specials transaction");
        return internal_error("Could not save specials");
    }

    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn update_fees_rejects_an_unrecognized_fee_type_without_touching_the_database() {
        let response = update_fees(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateFeesRequest {
                fees: vec![FeeInput {
                    fee_type: "not_a_real_type".to_string(),
                    label: None,
                    raw_value: "$10".to_string(),
                }],
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_fees_reaches_the_database() {
        let response = update_fees(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateFeesRequest { fees: vec![] }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn update_delinquency_rejects_an_unrecognized_step_type_without_touching_the_database() {
        let response = update_delinquency(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateDelinquencyRequest {
                steps: vec![DelinquencyStepInput {
                    step_order: 1,
                    step_type: "not_a_real_step".to_string(),
                    raw_value: "whatever".to_string(),
                }],
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_taxes_reaches_the_database() {
        let response = update_taxes(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateTaxesRequest {
                sales_tax_applies_raw: None,
                sales_tax_rate_raw: None,
                rent_tax_applies_raw: None,
                rent_tax_rate_raw: None,
                rent_tax_applies_to_all_units_raw: None,
                other_one_time_taxes_raw: None,
                other_recurring_taxes_raw: None,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn update_coverage_reaches_the_database() {
        let response = update_coverage(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateCoverageRequest { tiers: vec![], commission: None }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn update_specials_reaches_the_database() {
        let response = update_specials(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateSpecialsRequest { raw_text: None }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
