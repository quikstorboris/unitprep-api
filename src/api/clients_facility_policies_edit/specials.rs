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
pub struct UpdateSpecialsRequest {
    pub raw_text: Option<String>,
}

pub async fn update_specials(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<UpdateSpecialsRequest>,
) -> Response {
    let user_agent = request_context(&headers);

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

    let existing: Option<(Option<String>,)> = match sqlx::query_as(
        "SELECT raw_text FROM clients.policy_specials WHERE facility_policies_id = $1",
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
    let previous_raw_text = existing.and_then(|(raw_text,)| raw_text);

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

    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_SPECIALS_UPDATED,
        user.user_id,
        "facility_policies_specials",
        Some(&facility_id.to_string()),
        audit_log::Change::from_to(serde_json::json!(previous_raw_text), serde_json::json!(&request.raw_text)),
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

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
    async fn update_specials_reaches_the_database() {
        let response = update_specials(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(UpdateSpecialsRequest { raw_text: None }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
