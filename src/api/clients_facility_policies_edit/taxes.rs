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

use super::{bad_request, ensure_facility_and_policies_row, not_found, request_context};

const TAX_TYPES: &[&str] = &["fixed", "marginal", "percentage"];
const TAX_NAMES: &[&str] = &["sales", "rental"];

#[derive(Debug, Serialize, Deserialize)]
pub struct TaxEntryInput {
    pub tax_type: String,
    pub tax_name: String,
    pub description: Option<String>,
    pub flat_amount: Option<f64>,
    pub attribute_payable_percent: Option<f64>,
    pub is_recurring: bool,
}

#[derive(Debug, Serialize, Deserialize)]
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
    headers: HeaderMap,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateTaxesRequest>,
) -> Response {
    let user_agent = request_context(&headers);

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

    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_TAXES_UPDATED,
        user.user_id,
        "facility_policies_taxes",
        Some(&facility_id.to_string()),
        audit_log::Change { before: None, after: Some(serde_json::json!(request.taxes)) },
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit update_taxes transaction");
        return internal_error("Could not save taxes");
    }

    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn update_taxes_rejects_an_unrecognized_tax_name_without_touching_the_database() {
        let response = update_taxes(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
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
            HeaderMap::new(),
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
}
