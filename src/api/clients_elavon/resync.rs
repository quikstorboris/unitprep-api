//! `POST .../elavon/resync` -- re-pulls the linked run from Process Street.

use super::{not_linked, PERMISSION};
use crate::api::rls::{begin_for, try_response};
use crate::api::{
    encryption_not_configured, internal_error, not_found, process_street_not_configured,
    user_agent_from, AppState,
};
use crate::auth::AuthenticatedUser;
use crate::client_ops::audit_log;
use crate::clients::merchant_account_mapping::{
    credentials_added_to_qms_from_tasks, map_merchant_account_fields,
};
use crate::clients::ps_task_roles;
use crate::clients::repository::{
    resync_merchant_account_run, upsert_task_status, IngestMerchantAccountError,
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use uuid::Uuid;

/// Refreshes a linked facility's whole Elavon/Merchant Account picture
/// from Process Street -- rate provided, application status,
/// `credentials_added_to_qms`, financials, the 4 QMS/pinpad credential
/// fields, and parties. The fix for this data having no refresh path
/// short of a destructive unlink/relink once PS's own data changes
/// after the initial link -- e.g. the "Add Credentials to QMS"
/// checklist step gets completed sometime after this facility was
/// first linked, and `credentials_added_to_qms` (only ever set at link
/// time) is stuck showing "No" (2026-09-09). Fetches both
/// `get_run_form_fields` and `get_run_tasks`, same two calls
/// `link_facility_elavon` already makes -- see
/// `repository::resync_merchant_account_run`'s own doc comment for why
/// a full overwrite is safe here (nothing on this tab has a manual-edit
/// UI to protect, unlike Intake's own resync).
///
/// Requires `client_ops.perform`, same as link/unlink.
pub async fn resync_elavon_data(
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
            "resync_elavon_data",
            user_agent,
            None,
        )
        .await
    {
        return response;
    }

    let Some(client) = state.process_street.clone() else {
        return process_street_not_configured();
    };

    // --- Phase 1: look up the linked run id, in its own short
    // transaction -- same phased shape `link_facility_elavon` uses and
    // for the same reason (never hold a transaction across a PS round
    // trip). ---
    let ma_run_id = {
        let mut tx = try_response!(
            begin_for(
                &state,
                &user,
                "Could not resync this facility's Elavon data"
            )
            .await
        );

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
                tracing::error!(error = %err, user_id = %user.user_id, "facility existence check for elavon data resync failed");
                return internal_error("Could not resync this facility's Elavon data");
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
                tracing::error!(error = %err, user_id = %user.user_id, "facility_merchant_accounts lookup for data resync failed");
                return internal_error("Could not resync this facility's Elavon data");
            }
        };

        if let Err(err) = tx.commit().await {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to commit elavon data resync's pre-check transaction");
            return internal_error("Could not resync this facility's Elavon data");
        }

        match existing {
            Some((Some(ma_run_id),)) => ma_run_id,
            _ => return not_linked(),
        }
    };

    // --- Phase 2: the live Process Street round trip, with no
    // transaction open -- fields and tasks concurrently, same reasoning
    // as `link_facility_elavon`'s own doc comment. ---
    let (fields_result, tasks_result) = tokio::join!(
        client.get_run_form_fields(&ma_run_id),
        client.get_run_tasks(&ma_run_id)
    );

    let fields = match fields_result {
        Ok(fields) => fields,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, ma_run_id, "failed to fetch this run's fields from Process Street for a data resync");
            return internal_error("Could not fetch this facility's data from Process Street");
        }
    };
    let tasks = match tasks_result {
        Ok(tasks) => tasks,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, ma_run_id, "failed to fetch this run's tasks from Process Street for a data resync");
            return internal_error("Could not fetch this facility's data from Process Street");
        }
    };
    let mapped = map_merchant_account_fields(&fields);
    let qms_credential_task_names = match ps_task_roles::load_task_names_as(
        &state.db,
        user.user_id,
        &user.role_keys,
        ps_task_roles::QMS_CREDENTIALS_ROLE,
    )
    .await
    {
        Ok(names) => names,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to load the Process Street task mapping");
            return internal_error("Could not load the Process Street task mapping");
        }
    };
    let credentials_added_to_qms =
        credentials_added_to_qms_from_tasks(&tasks, &qms_credential_task_names);

    // --- Phase 3: the write, in a fresh transaction opened only now
    // that nothing left to do is network-bound. ---
    let mut tx = try_response!(
        begin_for(
            &state,
            &user,
            "Could not resync this facility's Elavon data"
        )
        .await
    );

    if let Err(err) = resync_merchant_account_run(
        &mut tx,
        facility_id,
        &mapped,
        &ma_run_id,
        credentials_added_to_qms,
    )
    .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, ma_run_id, "failed to resync this facility's Merchant Account data");
        return match err {
            IngestMerchantAccountError::Encryption(_) => encryption_not_configured(),
            IngestMerchantAccountError::Database(_) => {
                internal_error("Could not resync this facility's Elavon data")
            }
        };
    }

    if let Err(err) = upsert_task_status(&mut tx, facility_id, "merchant_account", &tasks).await {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, ma_run_id, "failed to upsert merchant_account task status during a data resync");
        return internal_error("Could not resync this facility's Elavon data");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit elavon data resync transaction");
        return internal_error("Could not resync this facility's Elavon data");
    }

    audit_log::record(
        &state.db,
        audit_log::event::ELAVON_DATA_RESYNCED,
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
        ma_run_id,
        "user resynced a facility's Elavon data"
    );

    StatusCode::NO_CONTENT.into_response()
}
