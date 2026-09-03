//! Facility page's Elavon tab -- Phase 4 item 5. `GET` shows whichever
//! is true: this facility already has a linked New Merchant Account
//! run (its own summary + parties, matching Company page's Owner(s)
//! Information but scoped to this one facility), or it doesn't, in
//! which case a title-correlation candidate is suggested the same way
//! `api::clients_search`/`api::clients_preview` already do it for a
//! not-yet-imported facility. `POST .../link` is the manual confirm
//! action for that candidate (or any run id the caller already knows) --
//! this is the general-purpose fix for what created Prairie/Highway
//! 20's own gap (2026-09-03): a facility created before its Merchant
//! Account run was ever correlated, or where auto-correlation simply
//! never found a match, previously had no way to get linked at all
//! short of a one-off backfill script. This tab is that path, built to
//! be used repeatedly, not just once.
//!
//! Deliberate friction, per the original design: the candidate is shown
//! with its own run name and PS run id so the caller can go verify it
//! in Process Street before confirming -- `link_facility_elavon` never
//! runs on its own, only on an explicit id the caller (or the candidate
//! suggestion) already named.

use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log;
use crate::clients::merchant_account_correlation::{
    correlate_by_title, merchant_account_run_titles, Correlation, IntakeRunTitle, MerchantAccountRunInfo,
};
use crate::clients::merchant_account_mapping::{
    credentials_added_to_qms_from_tasks, decrypt_facility_secrets, decrypt_party_pii, map_merchant_account_fields,
    mask_bank_number,
};
use crate::clients::repository::{ingest_merchant_account_run, upsert_task_status, IngestMerchantAccountError};

const PERMISSION: &str = "client_ops.perform";

fn not_found(entity: &'static str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ApiErrorBody { error: "not_found", message: format!("No such {entity}.") }),
    )
        .into_response()
}

fn already_linked() -> Response {
    (
        StatusCode::CONFLICT,
        Json(ApiErrorBody {
            error: "already_linked",
            message: "This facility already has a linked Merchant Account run.".to_string(),
        }),
    )
        .into_response()
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

fn request_context(headers: &HeaderMap) -> Option<&str> {
    headers.get(axum::http::header::USER_AGENT).and_then(|value| value.to_str().ok())
}

#[derive(Debug, Serialize)]
pub struct ElavonPartyInfo {
    pub party_role: String,
    pub display_name: Option<String>,
    pub title: Option<String>,
    pub ownership_percent: Option<f64>,
    pub email: Option<String>,
    pub phone: Option<String>,
    /// The real decrypted SSN -- masking (fully, with a Show/Hide reveal
    /// toggle) is deliberately a frontend concern (`PartyCard`), not done
    /// here, since Boris wants the real value revealable on demand.
    pub ssn: Option<String>,
    pub dob: Option<String>,
    pub home_address_line1: Option<String>,
    pub home_city: Option<String>,
    pub home_state_or_province: Option<String>,
    pub home_postal_code: Option<String>,
}

/// Facility-level financial data shown on the Elavon tab (2026-09-03) --
/// EIN and the bank routing/account numbers (decrypted from
/// `encrypted_secrets`, masked to their last 4 digits before leaving the
/// backend -- see `mask_bank_number`'s own doc comment), plus the
/// revenue/volume fields New Merchant Account's Facility Information
/// (Pre-App) step captures. `None` throughout when this facility has no
/// `encrypted_secrets` blob at all (a Merchant Account run ingested
/// before this field existed) or its decryption fails -- degrades this
/// one section rather than the whole tab, same pattern `ElavonPartyInfo`
/// already uses for a party's PII.
#[derive(Debug, Serialize, Default)]
pub struct ElavonFinancials {
    pub ein: Option<String>,
    pub bank_routing_number_masked: Option<String>,
    pub bank_account_number_masked: Option<String>,
    pub total_annual_business_revenue_raw: Option<String>,
    pub total_monthly_sales_raw: Option<String>,
    pub average_credit_card_payment_amount_raw: Option<String>,
    pub highest_credit_card_payment_amount_raw: Option<String>,
    pub high_cc_payment_times_per_year_raw: Option<String>,
    pub offers_ach_raw: Option<String>,
    pub annual_electronic_check_volume_raw: Option<String>,
    pub average_electronic_check_amount_raw: Option<String>,
    pub maximum_electronic_check_amount_raw: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ElavonCandidate {
    pub merchant_account_run_id: String,
    pub run_name: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ElavonStatusResponse {
    Linked {
        rate_provided: Option<String>,
        application_status: Option<String>,
        credentials_added_to_qms: bool,
        ps_new_merchant_run_id: Option<String>,
        last_synced_at: Option<chrono::DateTime<chrono::Utc>>,
        parties: Vec<ElavonPartyInfo>,
        // Boxed -- clippy::large_enum_variant. `ElavonFinancials` is 9
        // Option<String> fields plus 3 more, which otherwise makes this
        // variant nearly 4.5x the size of `Unlinked`, forcing every
        // `ElavonStatusResponse` (including the far more common Unlinked
        // case) to be sized for the largest variant.
        financials: Box<ElavonFinancials>,
    },
    Unlinked {
        /// Present when title correlation found exactly one candidate
        /// (see `Correlation::Unambiguous`). Absent -- not an error --
        /// when there's genuinely no candidate at all, or the match was
        /// ambiguous (`ambiguous_candidates` below is used instead in
        /// that case).
        candidate: Option<ElavonCandidate>,
        /// Populated instead of `candidate` when title correlation found
        /// more than one match (`Correlation::Ambiguous`) -- a real
        /// duplicate submission, confirmed against Carpentersville's own
        /// data (see `merchant_account_correlation`'s own module doc).
        /// Never auto-picked, but shown as real options rather than
        /// forcing pure manual entry -- same idea `clients_search`'s own
        /// "Potential Duplicates" rows use.
        ambiguous_candidates: Vec<ElavonCandidate>,
    },
}

#[derive(sqlx::FromRow)]
struct FacilityIdentity {
    ps_intake_run_id: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ExistingMerchantAccountRow {
    rate_provided: Option<String>,
    application_status: Option<String>,
    credentials_added_to_qms: bool,
    ps_new_merchant_run_id: Option<String>,
    last_synced_at: Option<chrono::DateTime<chrono::Utc>>,
    encrypted_secrets: Option<Vec<u8>>,
    total_annual_business_revenue_raw: Option<String>,
    total_monthly_sales_raw: Option<String>,
    average_credit_card_payment_amount_raw: Option<String>,
    highest_credit_card_payment_amount_raw: Option<String>,
    high_cc_payment_times_per_year_raw: Option<String>,
    offers_ach_raw: Option<String>,
    annual_electronic_check_volume_raw: Option<String>,
    average_electronic_check_amount_raw: Option<String>,
    maximum_electronic_check_amount_raw: Option<String>,
}

/// Decrypts `existing.encrypted_secrets` (when present) into the
/// financials shown on the Elavon tab -- EIN unmasked (not asked to be
/// masked, and less sensitive than a personal SSN/bank account), bank
/// routing/account numbers masked to their last 4 digits. A decrypt
/// failure degrades to `None` for just the EIN/bank fields (logged, not
/// surfaced to the caller) -- the revenue/volume fields below don't
/// depend on `encrypted_secrets` at all and are unaffected either way.
fn build_financials(facility_id: Uuid, existing: &ExistingMerchantAccountRow) -> ElavonFinancials {
    let secrets = existing.encrypted_secrets.as_deref().and_then(|blob| {
        match decrypt_facility_secrets(facility_id, blob) {
            Ok(secrets) => Some(secrets),
            Err(err) => {
                tracing::error!(
                    error = %err,
                    facility_id = %facility_id,
                    "failed to decrypt facility secrets for the Elavon tab"
                );
                None
            }
        }
    });

    ElavonFinancials {
        ein: secrets.as_ref().and_then(|s| s.ein.clone()),
        bank_routing_number_masked: secrets
            .as_ref()
            .and_then(|s| s.bank_routing_number.as_deref())
            .map(mask_bank_number),
        bank_account_number_masked: secrets
            .as_ref()
            .and_then(|s| s.bank_account_number.as_deref())
            .map(mask_bank_number),
        total_annual_business_revenue_raw: existing.total_annual_business_revenue_raw.clone(),
        total_monthly_sales_raw: existing.total_monthly_sales_raw.clone(),
        average_credit_card_payment_amount_raw: existing.average_credit_card_payment_amount_raw.clone(),
        highest_credit_card_payment_amount_raw: existing.highest_credit_card_payment_amount_raw.clone(),
        high_cc_payment_times_per_year_raw: existing.high_cc_payment_times_per_year_raw.clone(),
        offers_ach_raw: existing.offers_ach_raw.clone(),
        annual_electronic_check_volume_raw: existing.annual_electronic_check_volume_raw.clone(),
        average_electronic_check_amount_raw: existing.average_electronic_check_amount_raw.clone(),
        maximum_electronic_check_amount_raw: existing.maximum_electronic_check_amount_raw.clone(),
    }
}

#[derive(sqlx::FromRow)]
struct PartyRow {
    party_role: String,
    party_index: i32,
    display_name: Option<String>,
    title: Option<String>,
    ownership_percent: Option<f64>,
    email: Option<String>,
    phone: Option<String>,
    encrypted_pii: Option<Vec<u8>>,
}

fn decrypt_parties(facility_id: Uuid, rows: Vec<PartyRow>) -> Vec<ElavonPartyInfo> {
    rows.into_iter()
        .map(|row| {
            let pii = row.encrypted_pii.as_deref().and_then(|blob| {
                match decrypt_party_pii(facility_id, &row.party_role, row.party_index, blob) {
                    Ok(pii) => Some(pii),
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            facility_id = %facility_id,
                            party_role = %row.party_role,
                            party_index = row.party_index,
                            "failed to decrypt a party's PII for the Elavon tab"
                        );
                        None
                    }
                }
            });

            ElavonPartyInfo {
                party_role: row.party_role,
                display_name: row.display_name,
                title: row.title,
                ownership_percent: row.ownership_percent,
                email: row.email,
                phone: row.phone,
                ssn: pii.as_ref().and_then(|p| p.ssn.clone()),
                dob: pii.as_ref().and_then(|p| p.dob.clone()),
                home_address_line1: pii.as_ref().and_then(|p| p.home_address_line1.clone()),
                home_city: pii.as_ref().and_then(|p| p.home_city.clone()),
                home_state_or_province: pii.as_ref().and_then(|p| p.home_state_or_province.clone()),
                home_postal_code: pii.as_ref().and_then(|p| p.home_postal_code.clone()),
            }
        })
        .collect()
}

/// Any authenticated caller -- same reasoning as `clients_detail`'s own
/// module doc: the sensitive parts are protected by RLS itself
/// (`facility_merchant_accounts`/`facility_merchant_account_parties`
/// stay `onboarding_manager`/`department_manager`-only at the database
/// level).
pub async fn get_facility_elavon(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path((company_id, facility_id)): Path<(Uuid, Uuid)>,
) -> Response {
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for facility elavon");
            return internal_error("Could not load this facility's Elavon status");
        }
    };

    let facility: Option<FacilityIdentity> =
        match sqlx::query_as("SELECT ps_intake_run_id FROM clients.facilities WHERE id = $1 AND company_id = $2")
            .bind(facility_id)
            .bind(company_id)
            .fetch_optional(&mut *tx)
            .await
        {
            Ok(row) => row,
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "facility lookup for elavon failed");
                return internal_error("Could not load this facility's Elavon status");
            }
        };
    let Some(facility) = facility else {
        let _ = tx.commit().await;
        return not_found("facility");
    };

    let existing: Option<ExistingMerchantAccountRow> = match sqlx::query_as(
        "SELECT rate_provided, application_status, credentials_added_to_qms, ps_new_merchant_run_id, last_synced_at, \
         encrypted_secrets, total_annual_business_revenue_raw, total_monthly_sales_raw, \
         average_credit_card_payment_amount_raw, highest_credit_card_payment_amount_raw, \
         high_cc_payment_times_per_year_raw, offers_ach_raw, annual_electronic_check_volume_raw, \
         average_electronic_check_amount_raw, maximum_electronic_check_amount_raw \
         FROM clients.facility_merchant_accounts WHERE facility_id = $1",
    )
    .bind(facility_id)
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(row) => row,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "facility_merchant_accounts lookup failed");
            return internal_error("Could not load this facility's Elavon status");
        }
    };

    if let Some(existing) = existing {
        // ownership_percent is NUMERIC -- cast to float8, see
        // `clients_detail.rs`'s own identical query for why.
        let party_rows: Vec<PartyRow> = match sqlx::query_as(
            "SELECT party_role, party_index, display_name, title, ownership_percent::float8 AS ownership_percent, \
             email, phone, encrypted_pii \
             FROM clients.facility_merchant_account_parties \
             WHERE facility_id = $1 AND party_role IN ('owner', 'signer') ORDER BY party_index",
        )
        .bind(facility_id)
        .fetch_all(&mut *tx)
        .await
        {
            Ok(rows) => rows,
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "facility_merchant_account_parties lookup failed");
                return internal_error("Could not load this facility's Elavon status");
            }
        };

        if let Err(err) = tx.commit().await {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to commit facility elavon transaction");
            return internal_error("Could not load this facility's Elavon status");
        }

        let financials = Box::new(build_financials(facility_id, &existing));

        return Json(ElavonStatusResponse::Linked {
            rate_provided: existing.rate_provided,
            application_status: existing.application_status,
            credentials_added_to_qms: existing.credentials_added_to_qms,
            ps_new_merchant_run_id: existing.ps_new_merchant_run_id,
            last_synced_at: existing.last_synced_at,
            parties: decrypt_parties(facility_id, party_rows),
            financials,
        })
        .into_response();
    }

    // Not linked yet -- suggest a candidate the same way
    // `clients_search`/`clients_preview` already do, purely off already
    // locally-indexed data (no live PS call here).
    let (candidate, ambiguous_candidates) = match &facility.ps_intake_run_id {
        None => (None, Vec::new()),
        Some(intake_run_id) => {
            let intake_title: Option<(String,)> = match sqlx::query_as(
                "SELECT run_name FROM clients.ps_sync_state WHERE workflow = 'intake' AND ps_run_id = $1",
            )
            .bind(intake_run_id)
            .fetch_optional(&mut *tx)
            .await
            {
                Ok(row) => row,
                Err(err) => {
                    tracing::error!(error = %err, user_id = %user.user_id, "intake title lookup failed");
                    return internal_error("Could not load this facility's Elavon status");
                }
            };

            match intake_title {
                None => (None, Vec::new()),
                Some((title_text,)) => {
                    let ma_titles: Vec<MerchantAccountRunInfo> = match merchant_account_run_titles(&mut tx).await {
                        Ok(titles) => titles,
                        Err(err) => {
                            tracing::error!(error = %err, user_id = %user.user_id, "merchant account title fetch failed");
                            return internal_error("Could not load this facility's Elavon status");
                        }
                    };

                    let as_candidate = |ma_run_id: &str| {
                        ma_titles.iter().find(|ma| ma.run_id == ma_run_id).map(|ma| ElavonCandidate {
                            merchant_account_run_id: ma.run_id.clone(),
                            run_name: ma.run_name.clone(),
                            updated_at: ma.updated_at,
                        })
                    };

                    let intake_runs = [IntakeRunTitle { run_id: intake_run_id.clone(), title_text }];
                    let correlated = correlate_by_title(&intake_runs, &ma_titles);
                    match correlated.get(intake_run_id) {
                        Some(Correlation::Unambiguous(ma_run_id)) => (as_candidate(ma_run_id), Vec::new()),
                        Some(Correlation::Ambiguous(ma_run_ids)) => {
                            (None, ma_run_ids.iter().filter_map(|id| as_candidate(id)).collect())
                        }
                        None => (None, Vec::new()),
                    }
                }
            }
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit facility elavon transaction");
        return internal_error("Could not load this facility's Elavon status");
    }

    Json(ElavonStatusResponse::Unlinked { candidate, ambiguous_candidates }).into_response()
}

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
    let user_agent = request_context(&headers);

    if let Err(response) =
        user.require_permission(&state.db, PERMISSION, "link_facility_merchant_account", user_agent, None).await
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
        let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
            Ok(tx) => tx,
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for elavon link");
                return internal_error("Could not link this Merchant Account run");
            }
        };

        let facility_exists: Option<(Uuid,)> =
            match sqlx::query_as("SELECT id FROM clients.facilities WHERE id = $1 AND company_id = $2")
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
            return not_found("facility");
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
    let (fields_result, tasks_result) =
        tokio::join!(client.get_run_form_fields(ma_run_id), client.get_run_tasks(ma_run_id));

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
    let credentials_added_to_qms = credentials_added_to_qms_from_tasks(&tasks);

    // --- Phase 3: the actual write, in a fresh transaction opened only
    // now that nothing left to do is network-bound. A concurrent second
    // link request slipping in between Phase 1's check and this insert
    // is possible but rare (a manual, one-at-a-time admin action) and
    // self-corrects: `facility_merchant_accounts.facility_id` is a
    // primary key, so the loser gets a clean database error here rather
    // than corrupting anything. ---
    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for elavon link write");
            return internal_error("Could not link this Merchant Account run");
        }
    };

    if let Err(err) =
        ingest_merchant_account_run(&mut tx, facility_id, &mapped, ma_run_id, credentials_added_to_qms).await
    {
        let _ = tx.rollback().await;
        tracing::error!(error = %err, user_id = %user.user_id, ma_run_id, "failed to ingest linked Merchant Account run");
        return match err {
            IngestMerchantAccountError::Encryption(_) => encryption_not_configured(),
            IngestMerchantAccountError::Database(_) => internal_error("Could not link this Merchant Account run"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, test_user};

    #[tokio::test]
    async fn get_facility_elavon_reaches_the_database() {
        let response =
            get_facility_elavon(State(empty_state()), test_user(), Path((Uuid::new_v4(), Uuid::new_v4()))).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn link_facility_elavon_refuses_insufficient_permission_without_touching_anything() {
        let response = link_facility_elavon(
            State(empty_state()),
            test_user(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(LinkElavonRequest { merchant_account_run_id: "abc123".to_string() }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn link_facility_elavon_rejects_a_blank_run_id() {
        let response = link_facility_elavon(
            State(empty_state()),
            crate::api::test_support::onboarding_manager_user(),
            HeaderMap::new(),
            Path((Uuid::new_v4(), Uuid::new_v4())),
            Json(LinkElavonRequest { merchant_account_run_id: "   ".to_string() }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
