//! `GET` -- the Elavon tab's read: the linked run's summary, or a correlation
//! candidate to confirm.

use super::build::{
    build_credentials, build_financials, decrypt_parties, ExistingMerchantAccountRow,
    FacilityIdentity, PartyRow,
};
use super::dto::{ElavonCandidate, ElavonStatusResponse};
use crate::api::{internal_error, not_found, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::clients::merchant_account_correlation::{
    all_intake_run_titles, correlate_by_title, merchant_account_run_titles, Correlation,
    IntakeRunTitle, MerchantAccountRunInfo,
};
use axum::{
    extract::{Json, Path, State},
    response::{IntoResponse, Response},
};
use uuid::Uuid;

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

    let facility: Option<FacilityIdentity> = match sqlx::query_as(
        "SELECT ps_intake_run_id FROM clients.facilities WHERE id = $1 AND company_id = $2",
    )
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
        return not_found("not_found", "No such facility.".to_string());
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
        let (qms_credentials, pinpad_credentials) = build_credentials(facility_id, &existing);

        return Json(ElavonStatusResponse::Linked {
            rate_provided: existing.rate_provided,
            application_status: existing.application_status,
            credentials_added_to_qms: existing.credentials_added_to_qms,
            ps_new_merchant_run_id: existing.ps_new_merchant_run_id,
            last_synced_at: existing.last_synced_at,
            parties: decrypt_parties(facility_id, party_rows),
            financials,
            qms_credentials,
            pinpad_credentials,
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
                    let ma_titles: Vec<MerchantAccountRunInfo> = match merchant_account_run_titles(
                        &mut tx,
                    )
                    .await
                    {
                        Ok(titles) => titles,
                        Err(err) => {
                            tracing::error!(error = %err, user_id = %user.user_id, "merchant account title fetch failed");
                            return internal_error("Could not load this facility's Elavon status");
                        }
                    };

                    // Every sister's Intake title, so a keyword that names
                    // the company/owner (not this one facility) is ignored.
                    let intake_universe = match all_intake_run_titles(&mut tx).await {
                        Ok(titles) => titles,
                        Err(err) => {
                            tracing::error!(error = %err, user_id = %user.user_id, "intake title universe fetch failed");
                            return internal_error("Could not load this facility's Elavon status");
                        }
                    };

                    let as_candidate = |ma_run_id: &str| {
                        ma_titles
                            .iter()
                            .find(|ma| ma.run_id == ma_run_id)
                            .map(|ma| ElavonCandidate {
                                merchant_account_run_id: ma.run_id.clone(),
                                run_name: ma.run_name.clone(),
                                updated_at: ma.updated_at,
                            })
                    };

                    let intake_runs = [IntakeRunTitle {
                        run_id: intake_run_id.clone(),
                        title_text,
                    }];
                    let correlated = correlate_by_title(&intake_runs, &ma_titles, &intake_universe);
                    match correlated.get(intake_run_id) {
                        Some(Correlation::Unambiguous(ma_run_id)) => {
                            (as_candidate(ma_run_id), Vec::new())
                        }
                        Some(Correlation::Ambiguous(ma_run_ids)) => (
                            None,
                            ma_run_ids
                                .iter()
                                .filter_map(|id| as_candidate(id))
                                .collect(),
                        ),
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

    Json(ElavonStatusResponse::Unlinked {
        candidate,
        ambiguous_candidates,
    })
    .into_response()
}
