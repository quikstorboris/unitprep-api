//! Reads from the database and from Process Street: the company, its facilities, and each linked run's fields and tasks.

use super::rows::{CompanyRow, FacilityRow};
use crate::clients::merchant_account_mapping::{
    credentials_added_to_qms_from_tasks, map_merchant_account_fields, MappedMerchantAccount,
};
use crate::integrations::http::join_all_bounded;
use crate::process_street::{FormField, Task};
use sqlx::{Postgres, Transaction};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub(super) async fn fetch_company_and_facilities(
    tx: &mut Transaction<'_, Postgres>,
    company_id: Uuid,
) -> Result<Option<(CompanyRow, Vec<FacilityRow>)>, sqlx::Error> {
    let company: Option<CompanyRow> = sqlx::query_as(
        "SELECT id, ps_intake_run_id, legal_name, corporate_email, corporate_phone, \
         corporate_address_street, corporate_address_city, corporate_address_state, \
         corporate_address_zip, subdomain, accepted_payment_methods, accounting_basis, \
         payment_scheme, offers_tenant_insurance_raw, insurance_provider, website_url, \
         manually_edited_fields \
         FROM clients.companies WHERE id = $1",
    )
    .bind(company_id)
    .fetch_optional(&mut **tx)
    .await?;

    let Some(company) = company else {
        return Ok(None);
    };

    let facilities: Vec<FacilityRow> = sqlx::query_as(
        "SELECT id, ps_intake_run_id, name, street_address, city, state, zip, phone, email, \
         units_count, primary_storage_offering, previous_pms, access_control_system, \
         go_live_date, dropbox_folder_url, subdomain, subdomain_exists_in_qms_raw, \
         system_email, website_url, manually_edited_fields \
         FROM clients.facilities WHERE company_id = $1",
    )
    .bind(company_id)
    .fetch_all(&mut **tx)
    .await?;

    Ok(Some((company, facilities)))
}

/// Fetches every distinct PS run id this company/its facilities cite,
/// concurrently -- same `join_all` pattern `clients::create` and
/// `api::clients_preview` already use. A row with no `ps_intake_run_id`
/// at all (a manually-created client, `source = 'manual'`) is simply
/// skipped -- there is nothing in Process Street to refresh it against.
/// A single run failing to fetch degrades to "nothing to compare" for
/// just that one row rather than failing the whole request, same
/// resilience `clients_search`'s own company-name lookups already have.
pub(super) async fn fetch_fresh_fields(
    client: &crate::process_street::ProcessStreetClient,
    run_ids: HashSet<String>,
) -> HashMap<String, Vec<FormField>> {
    let fetches = run_ids.into_iter().map(|run_id| async move {
        let result = client.get_run_form_fields(&run_id).await;
        (run_id, result)
    });

    let mut fields_by_run_id = HashMap::new();
    for (run_id, result) in join_all_bounded(fetches).await {
        match result {
            Ok(fields) => {
                fields_by_run_id.insert(run_id, fields);
            }
            Err(err) => {
                tracing::warn!(error = %err, run_id, "failed to fetch a run's fields during Re-sync -- skipping it");
            }
        }
    }
    fields_by_run_id
}

/// A linked facility's freshly-fetched Merchant Account picture -- the
/// Elavon tab's own `credentials_added_to_qms`/task checklist previously
/// only ever refreshed via that tab's dedicated "Resync Elavon Data"
/// button (`api::clients_elavon::resync_elavon_data`), never via this
/// per-client Re-sync -- confirmed 2026-09-22 against a real stale case
/// (Main Street Storage's "Add Credentials to QMS" reverted in Process
/// Street 4 days after this facility's last Elavon-tab-specific resync,
/// and this button had no way to notice). No manual-edit protection
/// exists on this tab (same reasoning `resync_elavon_data`'s own doc
/// comment gives), so -- unlike Intake's company/facility fields -- this
/// is always a full overwrite, never a per-field conflict choice.
pub(super) struct MerchantAccountRefresh {
    pub(super) ps_new_merchant_run_id: String,
    pub(super) mapped: MappedMerchantAccount,
    pub(super) credentials_added_to_qms: bool,
    pub(super) tasks: Vec<Task>,
}

/// `clients.facility_merchant_accounts.ps_new_merchant_run_id` for every
/// one of this company's facilities that's actually linked to Elavon --
/// a facility with no row there (never linked) is simply absent from the
/// result, same "nothing to refresh against" resilience `fetch_fresh_fields`
/// already has for a missing `ps_intake_run_id`.
pub(super) async fn fetch_linked_merchant_account_runs(
    tx: &mut Transaction<'_, Postgres>,
    facility_ids: &[Uuid],
) -> Result<HashMap<Uuid, String>, sqlx::Error> {
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT facility_id, ps_new_merchant_run_id FROM clients.facility_merchant_accounts \
         WHERE facility_id = ANY($1) AND ps_new_merchant_run_id IS NOT NULL",
    )
    .bind(facility_ids)
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows.into_iter().collect())
}

/// Fetches each linked run's fields + tasks concurrently (same two calls
/// `link_facility_elavon`/`resync_elavon_data` already make), maps them,
/// and derives `credentials_added_to_qms` from the fresh task list -- a
/// run that fails to fetch degrades to "not refreshed this time" for
/// just that one facility, same resilience `fetch_fresh_fields` has.
pub(super) async fn fetch_fresh_merchant_account_data(
    client: &crate::process_street::ProcessStreetClient,
    run_ids_by_facility: HashMap<Uuid, String>,
    qms_credential_task_names: &[String],
) -> HashMap<Uuid, MerchantAccountRefresh> {
    let fetches = run_ids_by_facility
        .into_iter()
        .map(|(facility_id, run_id)| async move {
            let (fields_result, tasks_result) = tokio::join!(
                client.get_run_form_fields(&run_id),
                client.get_run_tasks(&run_id)
            );
            (facility_id, run_id, fields_result, tasks_result)
        });

    let mut refreshes = HashMap::new();
    for (facility_id, run_id, fields_result, tasks_result) in join_all_bounded(fetches).await {
        let fields = match fields_result {
            Ok(fields) => fields,
            Err(err) => {
                tracing::warn!(error = %err, run_id, "failed to fetch a Merchant Account run's fields during Re-sync -- skipping it");
                continue;
            }
        };
        let tasks = match tasks_result {
            Ok(tasks) => tasks,
            Err(err) => {
                tracing::warn!(error = %err, run_id, "failed to fetch a Merchant Account run's tasks during Re-sync -- skipping it");
                continue;
            }
        };

        let mapped = map_merchant_account_fields(&fields);
        let credentials_added_to_qms =
            credentials_added_to_qms_from_tasks(&tasks, qms_credential_task_names);
        refreshes.insert(
            facility_id,
            MerchantAccountRefresh {
                ps_new_merchant_run_id: run_id,
                mapped,
                credentials_added_to_qms,
                tasks,
            },
        );
    }
    refreshes
}
