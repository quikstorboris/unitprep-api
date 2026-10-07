//! `DELETE .../elavon` -- removes a facility's Merchant Account link and stored data.

use super::PERMISSION;
use crate::api::{internal_error, not_found, user_agent_from, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use uuid::Uuid;

/// Removes a facility's Merchant Account link entirely -- e.g. a wrong
/// run got linked (manually via the action above, or by automatic
/// correlation at Create time) and the manager needs to link the
/// correct one instead. Deletes `facility_merchant_accounts`, its party
/// rows, and its `ps_task_status` rows for this facility, all in one
/// transaction; a fresh `GET .../elavon` afterward goes back to the
/// unlinked view (candidate suggestion or manual entry), same as a
/// never-linked facility. No live PS call at all, unlike linking --
/// nothing here needs the phased pre-check/fetch/write split
/// `link_facility_elavon` uses to avoid holding a transaction across a
/// network round trip.
///
/// Requires `client_ops.perform`, same as linking.
pub async fn unlink_facility_elavon(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    if let Err(response) = user
        .require_permission(
            &state.db,
            PERMISSION,
            "unlink_facility_merchant_account",
            user_agent,
            None,
        )
        .await
    {
        return response;
    }

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for elavon unlink");
            return internal_error("Could not unlink this facility's Merchant Account run");
        }
    };

    let facility_exists: Option<(Uuid,)> = match sqlx::query_as(
        "SELECT id FROM clients.facilities WHERE id = $1 AND company_id = $2",
    )
    .bind(facility_id)
    .bind(company_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility existence check for elavon unlink failed");
            return internal_error("Could not unlink this facility's Merchant Account run");
        }
    };
    if facility_exists.is_none() {
        let _ = tx.rollback().await;
        return not_found("not_found", "No such facility.".to_string());
    }

    let existing: Option<(Option<String>,)> = match sqlx::query_as(
        "SELECT ps_new_merchant_run_id FROM clients.facility_merchant_accounts WHERE facility_id = $1",
    )
    .bind(facility_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility_merchant_accounts lookup for unlink failed");
            return internal_error("Could not unlink this facility's Merchant Account run");
        }
    };
    let Some((ma_run_id,)) = existing else {
        let _ = tx.rollback().await;
        return not_found(
            "not_found",
            "No such linked Merchant Account run.".to_string(),
        );
    };

    if let Err(err) =
        sqlx::query("DELETE FROM clients.facility_merchant_account_parties WHERE facility_id = $1")
            .bind(facility_id)
            .execute(&mut *tx)
            .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "failed to delete merchant account parties on unlink");
        return internal_error("Could not unlink this facility's Merchant Account run");
    }

    if let Err(err) =
        sqlx::query("DELETE FROM clients.ps_task_status WHERE facility_id = $1 AND workflow = 'merchant_account'")
            .bind(facility_id)
            .execute(&mut *tx)
            .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "failed to delete merchant_account task status on unlink");
        return internal_error("Could not unlink this facility's Merchant Account run");
    }

    if let Err(err) =
        sqlx::query("DELETE FROM clients.facility_merchant_accounts WHERE facility_id = $1")
            .bind(facility_id)
            .execute(&mut *tx)
            .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, "failed to delete facility_merchant_accounts on unlink");
        return internal_error("Could not unlink this facility's Merchant Account run");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit elavon unlink transaction");
        return internal_error("Could not unlink this facility's Merchant Account run");
    }

    audit_log::record(
        &state.db,
        audit_log::event::MERCHANT_ACCOUNT_UNLINKED,
        user.user_id,
        "facility",
        Some(&facility_id.to_string()),
        audit_log::Change::none(),
        user_agent,
        None,
        serde_json::json!({ "merchant_account_run_id": ma_run_id }),
    )
    .await;

    tracing::info!(
        user_id = %user.user_id,
        facility_id = %facility_id,
        ma_run_id = ?ma_run_id,
        "user manually unlinked a facility's Merchant Account run"
    );

    StatusCode::NO_CONTENT.into_response()
}
