use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{internal_error, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log;
use crate::clients::policy_exemption::{mark_exempt_if_qsx_and_was_empty, PolicyCategory};

use super::{ensure_facility_and_policies_row, not_found, request_context};

#[derive(Debug, Serialize, Deserialize)]
pub struct CoverageTierInput {
    pub tier_number: i32,
    pub total_coverage_amount_raw: Option<String>,
    pub cost_to_tenant_raw: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CommissionInput {
    pub commission_type_raw: Option<String>,
    pub dollar_amount_raw: Option<String>,
    pub percent_amount_raw: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
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
    headers: HeaderMap,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateCoverageRequest>,
) -> Response {
    let user_agent = request_context(&headers);

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

    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_COVERAGE_UPDATED,
        user.user_id,
        "facility_policies_coverage",
        Some(&facility_id.to_string()),
        audit_log::Change {
            before: None,
            after: Some(serde_json::json!({ "tiers": request.tiers, "commission": request.commission })),
        },
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit update_coverage transaction");
        return internal_error("Could not save coverage");
    }

    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn update_coverage_reaches_the_database() {
        let response = update_coverage(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateCoverageRequest { tiers: vec![], commission: None }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
