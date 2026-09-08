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
const TAX_TYPES: &[&str] = &["fixed", "marginal", "percentage"];
const TAX_NAMES: &[&str] = &["sales", "rental"];

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
pub struct TaxEntryInput {
    pub tax_type: String,
    pub tax_name: String,
    pub description: Option<String>,
    pub flat_amount: Option<f64>,
    pub attribute_payable_percent: Option<f64>,
    pub is_recurring: bool,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaxesRequest {
    pub taxes: Vec<TaxEntryInput>,
}

/// Structured, list-shaped taxes (2026-09-08), replacing the single
/// free-text `clients.policy_taxes` row this endpoint used to write --
/// see the migration's own doc comment for why the old table is left
/// alone (real historical data on Highway 20) rather than migrated.
/// Only `tax_type = "fixed"` is meaningful yet; "marginal"/"percentage"
/// are accepted (matching the DB's own CHECK) but have no dedicated
/// fields of their own so far -- deliberately deferred.
pub async fn update_taxes(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateTaxesRequest>,
) -> Response {
    if let Some(tax) = request.taxes.iter().find(|t| !TAX_TYPES.contains(&t.tax_type.as_str())) {
        return bad_request(format!("\"{}\" is not a recognized tax type.", tax.tax_type));
    }
    if let Some(tax) = request.taxes.iter().find(|t| !TAX_NAMES.contains(&t.tax_name.as_str())) {
        return bad_request(format!("\"{}\" is not a recognized tax name.", tax.tax_name));
    }

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

    let was_empty: (i64,) =
        match sqlx::query_as("SELECT count(*) FROM clients.policy_tax_entries WHERE facility_policies_id = $1")
            .bind(facility_id)
            .fetch_one(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(err) => {
                let _ = tx.rollback().await;
                tracing::error!(error = %err, user_id = %user.user_id, "policy_tax_entries count failed");
                return internal_error("Could not save taxes");
            }
        };
    let was_empty = was_empty.0 == 0;

    if let Err(err) = sqlx::query("DELETE FROM clients.policy_tax_entries WHERE facility_policies_id = $1")
        .bind(facility_id)
        .execute(&mut *tx)
        .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "policy_tax_entries delete failed");
        return internal_error("Could not save taxes");
    }

    for (index, tax) in request.taxes.iter().enumerate() {
        if let Err(err) = sqlx::query(
            "INSERT INTO clients.policy_tax_entries
                (facility_policies_id, tax_type, tax_name, description, flat_amount,
                 attribute_payable_percent, is_recurring, sort_order)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(facility_id)
        .bind(&tax.tax_type)
        .bind(&tax.tax_name)
        .bind(&tax.description)
        .bind(tax.flat_amount)
        .bind(tax.attribute_payable_percent)
        .bind(tax.is_recurring)
        .bind(index as i32 + 1)
        .execute(&mut *tx)
        .await
        {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "policy_tax_entries insert failed");
            return internal_error("Could not save taxes");
        }
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
pub struct DelinquencyEntryInput {
    pub category: String,
    pub name: String,
    pub amount: f64,
    pub days_after: Option<i32>,
    pub trigger_type: String,
    pub trigger_category: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDelinquencyRequest {
    pub entries: Vec<DelinquencyEntryInput>,
}

const TRIGGER_TYPES: &[&str] = &["paid_through_date", "step_category"];

/// Structured delinquency entries (2026-09-08), replacing the free-text
/// `clients.policy_delinquency_steps` this endpoint used to write --
/// see the migration's own doc comment for why the old table is left
/// alone (9 real historical rows on Highway 20, one naming two separate
/// fees in the same free-text value -- not something a migration can
/// safely split on its own). `trigger_category` references another
/// entry on this same facility's schedule by category rather than by
/// row id (Boris's own call, 2026-09-08: a schedule can't reasonably
/// have two rows in the same category, and category survives
/// reordering a row id wouldn't).
pub async fn update_delinquency(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateDelinquencyRequest>,
) -> Response {
    if let Some(entry) = request.entries.iter().find(|e| !STEP_TYPES.contains(&e.category.as_str())) {
        return bad_request(format!("\"{}\" is not a recognized delinquency category.", entry.category));
    }
    if let Some(entry) = request.entries.iter().find(|e| !TRIGGER_TYPES.contains(&e.trigger_type.as_str())) {
        return bad_request(format!("\"{}\" is not a recognized trigger type.", entry.trigger_type));
    }
    for entry in &request.entries {
        match entry.trigger_type.as_str() {
            "paid_through_date" if entry.trigger_category.is_some() => {
                return bad_request(
                    "trigger_category must not be set when trigger_type is \"paid_through_date\".".to_string(),
                );
            }
            "step_category" => match &entry.trigger_category {
                None => {
                    return bad_request(
                        "trigger_category is required when trigger_type is \"step_category\".".to_string(),
                    );
                }
                Some(trigger_category) => {
                    if !STEP_TYPES.contains(&trigger_category.as_str()) {
                        return bad_request(format!("\"{trigger_category}\" is not a recognized delinquency category."));
                    }
                    if trigger_category == &entry.category {
                        return bad_request("A delinquency entry cannot trigger off its own category.".to_string());
                    }
                }
            },
            _ => {}
        }
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for update_delinquency");
            return internal_error("Could not save delinquency entries");
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
            return internal_error("Could not save delinquency entries");
        }
    }

    let was_empty: (i64,) =
        match sqlx::query_as("SELECT count(*) FROM clients.policy_delinquency_entries WHERE facility_policies_id = $1")
            .bind(facility_id)
            .fetch_one(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(err) => {
                let _ = tx.rollback().await;
                tracing::error!(error = %err, user_id = %user.user_id, "policy_delinquency_entries count failed");
                return internal_error("Could not save delinquency entries");
            }
        };
    let was_empty = was_empty.0 == 0;

    if let Err(err) = sqlx::query("DELETE FROM clients.policy_delinquency_entries WHERE facility_policies_id = $1")
        .bind(facility_id)
        .execute(&mut *tx)
        .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "policy_delinquency_entries delete failed");
        return internal_error("Could not save delinquency entries");
    }

    for (index, entry) in request.entries.iter().enumerate() {
        if let Err(err) = sqlx::query(
            "INSERT INTO clients.policy_delinquency_entries
                (facility_policies_id, category, name, amount, days_after, trigger_type, trigger_category, sort_order)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(facility_id)
        .bind(&entry.category)
        .bind(&entry.name)
        .bind(entry.amount)
        .bind(entry.days_after)
        .bind(&entry.trigger_type)
        .bind(&entry.trigger_category)
        .bind(index as i32 + 1)
        .execute(&mut *tx)
        .await
        {
            let _ = tx.rollback().await;
            tracing::error!(error = %err, user_id = %user.user_id, "policy_delinquency_entries insert failed");
            return internal_error("Could not save delinquency entries");
        }
    }

    if let Err(err) =
        mark_exempt_if_qsx_and_was_empty(&mut tx, facility_id, PolicyCategory::Delinquency, was_empty).await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "delinquency exemption update failed");
        return internal_error("Could not save delinquency entries");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit update_delinquency transaction");
        return internal_error("Could not save delinquency entries");
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
    async fn update_delinquency_rejects_an_unrecognized_category_without_touching_the_database() {
        let response = update_delinquency(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateDelinquencyRequest {
                entries: vec![DelinquencyEntryInput {
                    category: "not_a_real_category".to_string(),
                    name: "1st Late Fee".to_string(),
                    amount: 10.0,
                    days_after: Some(7),
                    trigger_type: "paid_through_date".to_string(),
                    trigger_category: None,
                }],
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_delinquency_rejects_a_step_category_trigger_with_no_trigger_category_given() {
        let response = update_delinquency(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateDelinquencyRequest {
                entries: vec![DelinquencyEntryInput {
                    category: "lien".to_string(),
                    name: "Lien Fee".to_string(),
                    amount: 25.0,
                    days_after: Some(45),
                    trigger_type: "step_category".to_string(),
                    trigger_category: None,
                }],
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_delinquency_rejects_a_category_triggering_off_itself() {
        let response = update_delinquency(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateDelinquencyRequest {
                entries: vec![DelinquencyEntryInput {
                    category: "lien".to_string(),
                    name: "Lien Fee".to_string(),
                    amount: 25.0,
                    days_after: Some(45),
                    trigger_type: "step_category".to_string(),
                    trigger_category: Some("lien".to_string()),
                }],
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_delinquency_reaches_the_database() {
        let response = update_delinquency(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateDelinquencyRequest {
                entries: vec![DelinquencyEntryInput {
                    category: "pre_lien".to_string(),
                    name: "Pre-Lien Fee".to_string(),
                    amount: 0.0,
                    days_after: Some(30),
                    trigger_type: "paid_through_date".to_string(),
                    trigger_category: None,
                }],
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn update_taxes_rejects_an_unrecognized_tax_name_without_touching_the_database() {
        let response = update_taxes(
            State(empty_state()),
            test_user(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateTaxesRequest {
                taxes: vec![TaxEntryInput {
                    tax_type: "fixed".to_string(),
                    tax_name: "not_a_real_tax".to_string(),
                    description: None,
                    flat_amount: Some(0.0),
                    attribute_payable_percent: None,
                    is_recurring: false,
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
                taxes: vec![TaxEntryInput {
                    tax_type: "fixed".to_string(),
                    tax_name: "sales".to_string(),
                    description: Some("Sales tax".to_string()),
                    flat_amount: None,
                    attribute_payable_percent: Some(8.25),
                    is_recurring: true,
                }],
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
