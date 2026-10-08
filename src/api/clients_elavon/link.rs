//! `POST .../link` -- the manual confirm that ties a Merchant Account run to a facility.

use super::{already_linked, PERMISSION};
use crate::api::rls::{begin_for, try_response};
use crate::api::{
    encryption_not_configured, internal_error, not_found, process_street_not_configured,
    user_agent_from, ApiErrorBody, AppState,
};
use crate::auth::AuthenticatedUser;
use crate::client_ops::audit_log;
use crate::clients::merchant_account_mapping::{
    credentials_added_to_qms_from_tasks, map_merchant_account_fields,
};
use crate::clients::ps_task_roles;
use crate::clients::repository::{
    ingest_merchant_account_run, upsert_task_status, IngestMerchantAccountError,
};
use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct LinkElavonRequest {
    pub merchant_account_run_id: String,
}

/// Requires `client_ops.perform` -- same standing permission every other
/// client-data-mutating Process Street action uses (create, sync,
/// resync). Fetches `merchant_account_run_id` live from PS, maps it, and
/// ingests it exactly the way `clients::create` does for a brand-new
/// facility -- this is the same write, just triggered manually for a
/// facility that already exists.
pub async fn link_facility_elavon(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
    Json(request): Json<LinkElavonRequest>,
) -> Response {
    let user_agent = user_agent_from(&headers);

    if let Err(response) = user
        .require_permission(
            &state.db,
            PERMISSION,
            "link_facility_merchant_account",
            user_agent,
            None,
        )
        .await
    {
        return response;
    }

    let ma_run_id = request.merchant_account_run_id.trim();
    if ma_run_id.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(ApiErrorBody {
                error: "invalid_request",
                message: "merchant_account_run_id is required.".to_string(),
            }),
        )
            .into_response();
    }

    let Some(client) = state.process_street.clone() else {
        return process_street_not_configured();
    };

    // --- Phase 1: quick DB-only checks, in their own short transaction
    // that closes before anything talks to the network. ---
    {
        let mut tx = try_response!(
            begin_for(&state, &user, "Could not link this Merchant Account run").await
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
                tracing::error!(error = %err, user_id = %user.user_id, "facility existence check for elavon link failed");
                return internal_error("Could not link this Merchant Account run");
            }
        };
        if facility_exists.is_none() {
            let _ = tx.rollback().await;
            return not_found("not_found", "No such facility.".to_string());
        }

        // Guard against the unique-violation `ingest_merchant_account_run`
        // would otherwise hit -- it's a plain INSERT with no ON CONFLICT,
        // by design (a create-flow facility never already has one). A
        // manual re-link isn't supported this pass; unlink-then-relink is a
        // real future need but not today's.
        let already: Option<(Uuid,)> = match sqlx::query_as(
            "SELECT facility_id FROM clients.facility_merchant_accounts WHERE facility_id = $1",
        )
        .bind(facility_id)
        .fetch_optional(&mut *tx)
        .await
        {
            Ok(row) => row,
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "already-linked check failed");
                return internal_error("Could not link this Merchant Account run");
            }
        };
        if already.is_some() {
            let _ = tx.rollback().await;
            return already_linked();
        }

        if let Err(err) = tx.commit().await {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to commit elavon link's pre-check transaction");
            return internal_error("Could not link this Merchant Account run");
        }
    }

    // --- Phase 2: the live Process Street round trip, deliberately with
    // no open transaction (2026-09-03 fix -- this used to run inside the
    // same transaction Phase 1 used, holding a database connection and
    // its lock for however long PS took to answer; a slow PS response or
    // a cancelled request left the connection stuck `idle in
    // transaction`, blocking unrelated queries -- including session
    // lookups on `/clients` -- until someone manually killed it). Fields
    // and tasks fetched concurrently -- same reasoning as
    // `clients_detail`'s own doc comment on why independent reads
    // shouldn't wait on each other. `credentials_added_to_qms` needs
    // tasks (it's a checklist step, not a form field -- see
    // `merchant_account_mapping::credentials_added_to_qms_from_tasks`). ---
    let (fields_result, tasks_result) = tokio::join!(
        client.get_run_form_fields(ma_run_id),
        client.get_run_tasks(ma_run_id)
    );

    let fields = match fields_result {
        Ok(fields) => fields,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, ma_run_id, "failed to fetch Merchant Account run from Process Street");
            return internal_error("Could not fetch this run from Process Street");
        }
    };
    let tasks = match tasks_result {
        Ok(tasks) => tasks,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, ma_run_id, "failed to fetch this run's tasks from Process Street");
            return internal_error("Could not fetch this run from Process Street");
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

    // --- Phase 3: the actual write, in a fresh transaction opened only
    // now that nothing left to do is network-bound. A concurrent second
    // link request slipping in between Phase 1's check and this insert
    // is possible but rare (a manual, one-at-a-time admin action) and
    // self-corrects: `facility_merchant_accounts.facility_id` is a
    // primary key, so the loser gets a clean database error here rather
    // than corrupting anything. ---
    let mut tx =
        try_response!(begin_for(&state, &user, "Could not link this Merchant Account run").await);

    if let Err(err) = ingest_merchant_account_run(
        &mut tx,
        facility_id,
        &mapped,
        ma_run_id,
        credentials_added_to_qms,
    )
    .await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, ma_run_id, "failed to ingest linked Merchant Account run");
        return match err {
            IngestMerchantAccountError::Encryption(_) => encryption_not_configured(),
            IngestMerchantAccountError::Database(_) => {
                internal_error("Could not link this Merchant Account run")
            }
        };
    }

    if let Err(err) = upsert_task_status(&mut tx, facility_id, "merchant_account", &tasks).await {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, ma_run_id, "failed to upsert merchant_account task status");
        return internal_error("Could not link this Merchant Account run");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit elavon link transaction");
        return internal_error("Could not link this Merchant Account run");
    }

    audit_log::record(
        &state.db,
        audit_log::event::MERCHANT_ACCOUNT_LINKED,
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
        "user manually linked a Merchant Account run to a facility"
    );

    StatusCode::NO_CONTENT.into_response()
}
