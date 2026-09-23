//! Company page's "Manual Link" button -- lets a manager directly
//! repoint one facility's Intake or Merchant Account Process Street run
//! to a different run id, in place, without deleting and recreating the
//! client record. Built 2026-09-23 after Knapp's Self Stor of Milton
//! Freewater ended up linked to a different real business's Merchant
//! Account run ("Milton Self Storage") on nothing but a fuzzy
//! title/DBA text match -- see the vault's own write-up.
//!
//! Distinct from the Elavon tab's own "Link Manually"
//! (`api::clients_elavon::link_facility_elavon`), which only covers the
//! not-yet-linked case for Merchant Account specifically -- that
//! module's own doc comment explicitly deferred relinking-over-an-
//! existing-link ("a manual re-link isn't supported this pass"). This
//! endpoint covers both workflows, and, critically, relinking over an
//! already-linked run: for Merchant Account that means clearing the old
//! link's rows first (same three deletes `unlink_facility_elavon`
//! uses); for Intake there is no separate "already linked" state to
//! begin with -- every facility already has *some* Intake run from
//! creation, so this is always a relink in that case.
//!
//! Always a full overwrite, no `manually_edited_fields` protection --
//! same reasoning as Elavon's own link/resync actions: this is an
//! explicit, deliberate correction, not routine sync, and the facility
//! is about to start sourcing from a genuinely different PS run, so
//! carrying field-level protections over from the old (wrong) run
//! makes no sense.
//!
//! **Scope note, Intake side**: only refreshes the facility row's own
//! core fields (name, address, phone, etc.) plus `ps_person_index` for
//! the newly-linked run -- not `facility_policies`/fees/taxes/coverage/
//! specials, and not company-level fields. This mirrors
//! `api::clients_resync`'s own facility refresh, which has the same
//! scope; a relink is a correction to *which run* a facility's core
//! record and Merchant Account application come from, not a full
//! reimport of every policy table.

use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::api::{internal_error, not_found, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log;
use crate::clients::intake_mapping::{map_intake_fields, MappedFacility};
use crate::clients::merchant_account_mapping::{
    credentials_added_to_qms_from_tasks, map_merchant_account_fields,
};
use crate::clients::person_index::extract_intake_people;
use crate::clients::repository::{ingest_merchant_account_run, upsert_task_status, IngestMerchantAccountError};
use crate::clients::sync::apply_facility_refresh;

const PERMISSION: &str = "client_ops.perform";

fn request_context(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
}

fn process_street_not_configured() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiErrorBody {
            error: "process_street_not_configured",
            message: "Process Street integration is not configured on this server.".to_string(),
        }),
    )
        .into_response()
}

fn encryption_not_configured() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ApiErrorBody {
            error: "encryption_not_configured",
            message: "CLIENT_PII_ENCRYPTION_KEY is not configured on this server.".to_string(),
        }),
    )
        .into_response()
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiErrorBody {
            error: "invalid_request",
            message: message.to_string(),
        }),
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualLinkWorkflow {
    Intake,
    MerchantAccount,
}

#[derive(Debug, Deserialize)]
pub struct ManualLinkRequest {
    pub facility_id: Uuid,
    pub workflow: ManualLinkWorkflow,
    pub run_id: String,
}

#[derive(Debug, Serialize)]
pub struct ManualLinkResponse {
    pub workflow: &'static str,
}

async fn facility_belongs_to_company(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    facility_id: Uuid,
    company_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM clients.facilities WHERE id = $1 AND company_id = $2")
            .bind(facility_id)
            .bind(company_id)
            .fetch_optional(&mut **tx)
            .await?;

    Ok(row.is_some())
}

pub async fn manual_link(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    headers: HeaderMap,
    Path(company_id): Path<Uuid>,
    Json(request): Json<ManualLinkRequest>,
) -> Response {
    let user_agent = request_context(&headers);

    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "manual_link", user_agent, None)
        .await
    {
        return response;
    }

    let run_id = request.run_id.trim().to_string();
    if run_id.is_empty() {
        return bad_request("A Process Street run id is required.");
    }

    let Some(client) = state.process_street.clone() else {
        return process_street_not_configured();
    };

    // Facility-existence check, in its own short transaction that closes
    // before anything talks to the network -- same phased shape
    // `link_facility_elavon` uses and for the same reason (never hold a
    // database connection open across a live PS round trip).
    {
        let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
            Ok(tx) => tx,
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for manual link precheck");
                return internal_error("Could not link this run");
            }
        };

        match facility_belongs_to_company(&mut tx, request.facility_id, company_id).await {
            Ok(true) => {}
            Ok(false) => {
                let _ = tx.rollback().await;
                return not_found("not_found", "No such facility.".to_string());
            }
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for manual link failed");
                return internal_error("Could not link this run");
            }
        }

        if let Err(err) = tx.commit().await {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to commit manual link precheck transaction");
            return internal_error("Could not link this run");
        }
    }

    match request.workflow {
        ManualLinkWorkflow::MerchantAccount => {
            manual_link_merchant_account(&state, &user, user_agent, &client, request.facility_id, &run_id).await
        }
        ManualLinkWorkflow::Intake => {
            manual_link_intake(&state, &user, user_agent, &client, request.facility_id, &run_id).await
        }
    }
}

async fn manual_link_merchant_account(
    state: &AppState,
    user: &AuthenticatedUser,
    user_agent: Option<&str>,
    client: &crate::process_street::ProcessStreetClient,
    facility_id: Uuid,
    run_id: &str,
) -> Response {
    // Live Process Street round trip first, deliberately with no open
    // transaction -- same reasoning as `link_facility_elavon`'s own
    // Phase 2.
    let (fields_result, tasks_result) =
        tokio::join!(client.get_run_form_fields(run_id), client.get_run_tasks(run_id));

    let fields = match fields_result {
        Ok(fields) => fields,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, run_id, "failed to fetch Merchant Account run from Process Street");
            return internal_error("Could not fetch this run from Process Street");
        }
    };
    let tasks = match tasks_result {
        Ok(tasks) => tasks,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, run_id, "failed to fetch this run's tasks from Process Street");
            return internal_error("Could not fetch this run from Process Street");
        }
    };
    let mapped = map_merchant_account_fields(&fields);
    let credentials_added_to_qms = credentials_added_to_qms_from_tasks(&tasks);

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for manual link write");
            return internal_error("Could not link this run");
        }
    };

    // Whatever's already linked gets cleared first -- same three deletes
    // `unlink_facility_elavon` uses -- so this always succeeds as a
    // clean relink instead of hitting `facility_merchant_accounts`'s own
    // primary-key conflict on a plain insert.
    let previous_run_id: Option<(Option<String>,)> = match sqlx::query_as(
        "SELECT ps_new_merchant_run_id FROM clients.facility_merchant_accounts WHERE facility_id = $1",
    )
    .bind(facility_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "existing-link lookup for manual link failed");
            let _ = tx.rollback().await;
            return internal_error("Could not link this run");
        }
    };

    if previous_run_id.is_some() {
        for statement in [
            "DELETE FROM clients.facility_merchant_account_parties WHERE facility_id = $1",
            "DELETE FROM clients.ps_task_status WHERE facility_id = $1 AND workflow = 'merchant_account'",
            "DELETE FROM clients.facility_merchant_accounts WHERE facility_id = $1",
        ] {
            if let Err(err) = sqlx::query(statement).bind(facility_id).execute(&mut *tx).await {
                tracing::error!(error = %err, user_id = %user.user_id, "failed to clear the previous Merchant Account link during a manual relink");
                let _ = tx.rollback().await;
                return internal_error("Could not link this run");
            }
        }
    }

    if let Err(err) =
        ingest_merchant_account_run(&mut tx, facility_id, &mapped, run_id, credentials_added_to_qms).await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, run_id, "failed to ingest a manually-relinked Merchant Account run");
        return match err {
            IngestMerchantAccountError::Encryption(_) => encryption_not_configured(),
            IngestMerchantAccountError::Database(_) => internal_error("Could not link this run"),
        };
    }

    if let Err(err) = upsert_task_status(&mut tx, facility_id, "merchant_account", &tasks).await {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, run_id, "failed to upsert task status for a manually-relinked Merchant Account run");
        return internal_error("Could not link this run");
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit manual link transaction");
        return internal_error("Could not link this run");
    }

    if let Some(previous_run_id) = previous_run_id.and_then(|(id,)| id) {
        audit_log::record(
            &state.db,
            audit_log::event::MERCHANT_ACCOUNT_UNLINKED,
            user.user_id,
            "facility",
            Some(&facility_id.to_string()),
            audit_log::Change::none(),
            user_agent,
            None,
            serde_json::json!({ "merchant_account_run_id": previous_run_id, "reason": "manual_relink" }),
        )
        .await;
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
        serde_json::json!({ "merchant_account_run_id": run_id, "source": "manual_link" }),
    )
    .await;

    tracing::info!(user_id = %user.user_id, facility_id = %facility_id, run_id, "user manually linked a Merchant Account run via the Company page");

    Json(ManualLinkResponse {
        workflow: "merchant_account",
    })
    .into_response()
}

async fn manual_link_intake(
    state: &AppState,
    user: &AuthenticatedUser,
    user_agent: Option<&str>,
    client: &crate::process_street::ProcessStreetClient,
    facility_id: Uuid,
    run_id: &str,
) -> Response {
    let fields = match client.get_run_form_fields(run_id).await {
        Ok(fields) => fields,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, run_id, "failed to fetch Intake run from Process Street");
            return internal_error("Could not fetch this run from Process Street");
        }
    };

    let mapped = map_intake_fields(&fields);
    let people = extract_intake_people(&fields);
    let snapshot: Value = serde_json::to_value(&fields).unwrap_or(Value::Null);
    let refreshed = apply_facility_refresh(&MappedFacility::default(), &mapped.facility, &[]);

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for manual link write");
            return internal_error("Could not link this run");
        }
    };

    let previous_run_id: Option<(Option<String>,)> =
        match sqlx::query_as("SELECT ps_intake_run_id FROM clients.facilities WHERE id = $1")
            .bind(facility_id)
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "existing-link lookup for manual intake link failed");
                let _ = tx.rollback().await;
                return internal_error("Could not link this run");
            }
        };

    let result = sqlx::query(
        "UPDATE clients.facilities SET name = $1, street_address = $2, city = $3, state = $4, \
         zip = $5, phone = $6, email = $7, units_count = $8, primary_storage_offering = $9, \
         previous_pms = $10, access_control_system = $11, dropbox_folder_url = $12, \
         subdomain = $13, subdomain_exists_in_qms_raw = $14, system_email = $15, \
         website_url = $16, ps_intake_run_id = $17, raw_ps_snapshot = $18, \
         manually_edited_fields = '{}', last_synced_at = now() WHERE id = $19",
    )
    .bind(refreshed.name.as_deref().unwrap_or("(unnamed facility)"))
    .bind(&refreshed.street_address)
    .bind(&refreshed.city)
    .bind(&refreshed.state)
    .bind(&refreshed.zip)
    .bind(&refreshed.phone)
    .bind(&refreshed.email)
    .bind(refreshed.units_count)
    .bind(&refreshed.primary_storage_offering)
    .bind(&refreshed.previous_pms)
    .bind(&refreshed.access_control_system)
    .bind(&refreshed.dropbox_folder_url)
    .bind(&refreshed.subdomain)
    .bind(&refreshed.subdomain_exists_in_qms_raw)
    .bind(&refreshed.system_email)
    .bind(&refreshed.website_url)
    .bind(run_id)
    .bind(&snapshot)
    .bind(facility_id)
    .execute(&mut *tx)
    .await;

    if let Err(err) = result {
        tracing::error!(error = %err, user_id = %user.user_id, run_id, "failed to update a facility during a manual Intake relink");
        let _ = tx.rollback().await;
        return internal_error("Could not link this run");
    }

    // Same rebuild-wholesale, delete-then-insert shape `apply_resync`
    // already uses for `ps_person_index` -- this facility's Users tab
    // candidates should come from the newly-linked run, not the old one.
    if let Err(err) = sqlx::query("DELETE FROM clients.ps_person_index WHERE workflow = 'intake' AND ps_run_id = $1")
        .bind(run_id)
        .execute(&mut *tx)
        .await
    {
        tracing::error!(error = %err, user_id = %user.user_id, run_id, "failed to clear ps_person_index during a manual Intake relink");
        let _ = tx.rollback().await;
        return internal_error("Could not link this run");
    }

    for person in &people {
        if let Err(err) = sqlx::query(
            "INSERT INTO clients.ps_person_index (workflow, ps_run_id, run_name, full_name, email, phone, role) \
             VALUES ('intake', $1, $2, $3, $4, $5, $6)",
        )
        .bind(run_id)
        .bind(refreshed.name.as_deref().unwrap_or(run_id))
        .bind(&person.full_name)
        .bind(&person.email)
        .bind(&person.phone)
        .bind(person.role)
        .execute(&mut *tx)
        .await
        {
            tracing::error!(error = %err, user_id = %user.user_id, run_id, "failed to index a person during a manual Intake relink");
            let _ = tx.rollback().await;
            return internal_error("Could not link this run");
        }
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit manual intake link transaction");
        return internal_error("Could not link this run");
    }

    audit_log::record(
        &state.db,
        audit_log::event::FACILITY_INTAKE_RELINKED,
        user.user_id,
        "facility",
        Some(&facility_id.to_string()),
        audit_log::Change::from_to(
            serde_json::json!({ "ps_intake_run_id": previous_run_id.and_then(|(id,)| id) }),
            serde_json::json!({ "ps_intake_run_id": run_id }),
        ),
        user_agent,
        None,
        serde_json::json!({}),
    )
    .await;

    tracing::info!(user_id = %user.user_id, facility_id = %facility_id, run_id, "user manually relinked a facility's Intake run via the Company page");

    Json(ManualLinkResponse { workflow: "intake" }).into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn manual_link_refuses_insufficient_permission_without_touching_anything() {
        let response = manual_link(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            Json(ManualLinkRequest {
                facility_id: Uuid::new_v4(),
                workflow: ManualLinkWorkflow::MerchantAccount,
                run_id: "abc123".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn manual_link_rejects_a_blank_run_id() {
        let response = manual_link(
            State(empty_state()),
            crate::api::test_support::onboarding_manager_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            Json(ManualLinkRequest {
                facility_id: Uuid::new_v4(),
                workflow: ManualLinkWorkflow::Intake,
                run_id: "   ".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // No "reaches the database" test here -- `empty_state()`'s
    // `process_street: None` means this handler always returns
    // `process_street_not_configured()` (503) right after the blank-run-id
    // check, before ever opening a transaction, same as
    // `link_facility_elavon`'s own two tests being the only ones it has.
    #[tokio::test]
    async fn manual_link_reports_not_configured_with_sufficient_permission() {
        let response = manual_link(
            State(empty_state()),
            crate::api::test_support::onboarding_manager_user(),
            HeaderMap::new(),
            Path(Uuid::new_v4()),
            Json(ManualLinkRequest {
                facility_id: Uuid::new_v4(),
                workflow: ManualLinkWorkflow::MerchantAccount,
                run_id: "abc123".to_string(),
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
