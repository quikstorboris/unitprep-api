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

const STEP_TYPES: &[&str] = &["late_fee", "pre_lien", "lien", "cut_lock", "auction", "notice", "other"];
const TRIGGER_TYPES: &[&str] = &["paid_through_date", "step_category"];

#[derive(Debug, Serialize, Deserialize)]
pub struct DelinquencyEntryInput {
    pub category: String,
    pub name: String,
    pub amount: f64,
    pub days_after: Option<i32>,
    pub trigger_type: String,
    pub trigger_category: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateDelinquencyRequest {
    pub entries: Vec<DelinquencyEntryInput>,
}

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
    headers: HeaderMap,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateDelinquencyRequest>,
) -> Response {
    let user_agent = request_context(&headers);

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

    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_DELINQUENCY_UPDATED,
        user.user_id,
        "facility_policies_delinquency",
        Some(&facility_id.to_string()),
        audit_log::Change { before: None, after: Some(serde_json::json!(request.entries)) },
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit update_delinquency transaction");
        return internal_error("Could not save delinquency entries");
    }

    StatusCode::NO_CONTENT.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn update_delinquency_rejects_an_unrecognized_category_without_touching_the_database() {
        let response = update_delinquency(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
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
            HeaderMap::new(),
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
            HeaderMap::new(),
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
            HeaderMap::new(),
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
}
