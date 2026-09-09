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

const FEE_TYPES: &[&str] = &["security_deposit", "nsf_chargeback", "move_in_admin", "transfer", "cleaning", "other"];

#[derive(Debug, Serialize, Deserialize)]
pub struct FeeInput {
    pub fee_type: String,
    pub label: Option<String>,
    pub raw_value: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateFeesRequest {
    pub fees: Vec<FeeInput>,
}

pub async fn update_fees(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateFeesRequest>,
) -> Response {
    let user_agent = request_context(&headers);

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

    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_FEES_UPDATED,
        user.user_id,
        "facility_policies_fees",
        Some(&facility_id.to_string()),
        audit_log::Change { before: None, after: Some(serde_json::json!(request.fees)) },
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit update_fees transaction");
        return internal_error("Could not save fees");
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
            HeaderMap::new(),
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
            HeaderMap::new(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateFeesRequest { fees: vec![] }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
