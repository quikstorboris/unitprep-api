//! The database writes `apply_resync` performs inside its one transaction,
//! one function per kind of thing it updates: the company row, the
//! facility rows, the `ps_person_index` rebuild and the Merchant Account
//! refresh. Each takes the open transaction and returns what it changed or
//! an [`ApplyError`] naming the step, so the handler logs, rolls back and
//! answers once instead of repeating that after every statement.

use std::collections::HashMap;

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::apply::{effective_protected_fields, ConflictResolution};
use super::compare::{CompanyComparison, FacilityComparison};
use super::fetch::MerchantAccountRefresh;
use crate::clients::person_index::ExtractedPerson;
use crate::clients::repository::{
    resync_merchant_account_run, upsert_task_status, IngestMerchantAccountError,
};
use crate::clients::sync::{apply_company_refresh, apply_facility_refresh};

/// A write step failed. The variant says which, so the handler can log
/// the same message each step always logged and pick the same response.
pub(super) enum ApplyError {
    Database {
        step: &'static str,
        error: sqlx::Error,
    },
    MerchantAccount {
        facility_id: Uuid,
        error: IngestMerchantAccountError,
    },
    MerchantTasks {
        facility_id: Uuid,
        error: sqlx::Error,
    },
}

/// What a successful apply changed.
pub(super) struct Written {
    pub(super) updated_count: usize,
    pub(super) people_indexed: usize,
    pub(super) merchant_accounts_refreshed: usize,
}

/// Writes the company row if Process Street's values (or the resolved
/// protected-field set) differ from what's stored. Returns whether it wrote.
pub(super) async fn update_company(
    tx: &mut Transaction<'_, Postgres>,
    company: &CompanyComparison,
    resolutions: &[ConflictResolution],
) -> Result<bool, ApplyError> {
    let Some(fresh) = &company.fresh else {
        return Ok(false);
    };
    let effective_protected = effective_protected_fields(
        &company.row.manually_edited_fields,
        resolutions,
        "company",
        company.row.id,
    );
    let current = company.row.mapped();
    let refreshed = apply_company_refresh(&current, fresh, &effective_protected);

    if refreshed == current && effective_protected == company.row.manually_edited_fields {
        return Ok(false);
    }

    let legal_name = refreshed
        .legal_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("(unnamed company)");

    sqlx::query(
        "UPDATE clients.companies SET legal_name = $1, corporate_email = $2, corporate_phone = $3, \
         corporate_address_street = $4, corporate_address_city = $5, corporate_address_state = $6, \
         corporate_address_zip = $7, subdomain = $8, accepted_payment_methods = $9, \
         accounting_basis = $10, payment_scheme = $11, offers_tenant_insurance_raw = $12, \
         insurance_provider = $13, website_url = $14, manually_edited_fields = $15, \
         last_synced_at = now() \
         WHERE id = $16",
    )
    .bind(legal_name)
    .bind(&refreshed.corporate_email)
    .bind(&refreshed.corporate_phone)
    .bind(&refreshed.corporate_address_street)
    .bind(&refreshed.corporate_address_city)
    .bind(&refreshed.corporate_address_state)
    .bind(&refreshed.corporate_address_zip)
    .bind(&refreshed.subdomain)
    .bind(&refreshed.accepted_payment_methods)
    .bind(&refreshed.accounting_basis)
    .bind(&refreshed.payment_scheme)
    .bind(&refreshed.offers_tenant_insurance_raw)
    .bind(&refreshed.insurance_provider)
    .bind(&refreshed.website_url)
    .bind(&effective_protected)
    .bind(company.row.id)
    .execute(&mut **tx)
    .await
    .map_err(|error| ApplyError::Database {
        step: "update company",
        error,
    })?;

    Ok(true)
}

/// Writes every facility row whose values or protected-field set changed.
/// Returns how many it wrote.
pub(super) async fn update_facilities(
    tx: &mut Transaction<'_, Postgres>,
    facilities: &[FacilityComparison],
    resolutions: &[ConflictResolution],
) -> Result<usize, ApplyError> {
    let mut updated = 0;

    for facility in facilities {
        let Some(fresh) = &facility.fresh else {
            continue;
        };
        let effective_protected = effective_protected_fields(
            &facility.row.manually_edited_fields,
            resolutions,
            "facility",
            facility.row.id,
        );
        let current = facility.row.mapped();
        let refreshed = apply_facility_refresh(&current, fresh, &effective_protected);

        if refreshed == current && effective_protected == facility.row.manually_edited_fields {
            continue;
        }

        sqlx::query(
            "UPDATE clients.facilities SET name = $1, street_address = $2, city = $3, state = $4, \
             zip = $5, phone = $6, email = $7, units_count = $8, primary_storage_offering = $9, \
             previous_pms = $10, access_control_system = $11, dropbox_folder_url = $12, \
             subdomain = $13, subdomain_exists_in_qms_raw = $14, system_email = $15, \
             website_url = $16, manually_edited_fields = $17, last_synced_at = now() WHERE id = $18",
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
        .bind(&effective_protected)
        .bind(facility.row.id)
        .execute(&mut **tx)
        .await
        .map_err(|error| ApplyError::Database {
            step: "update a facility",
            error,
        })?;

        updated += 1;
    }

    Ok(updated)
}

/// Refresh `clients.ps_person_index` for every run just fetched --
/// the Users tab's own "Add User" candidates (`api::clients_facility_people`)
/// are sourced entirely from that table, and until this it was only
/// ever kept fresh by the separate scheduled/"Sync Now" background
/// sync, never by this per-client button despite its own doc comment
/// claiming a full re-pull. Same rebuild-wholesale, delete-then-insert
/// shape `apply_fetched_runs` already uses -- a genuine `run_name` value is
/// only available from `ps_sync_state` (that data isn't part of the
/// form-fields fetch this endpoint already makes), so a run with no
/// prior sync_state row at all falls back to the entity's own current
/// name rather than leaving `run_name` unset (NOT NULL).
///
/// One DELETE for every refreshed run, then one multi-row INSERT -- two
/// statements however many runs and people there are. This used to be a
/// DELETE per run and an INSERT per person, each its own round trip
/// inside this transaction: a company with ten runs and a few people
/// each paid sixty-odd network round trips here before committing.
/// Returns how many people were indexed.
pub(super) async fn rebuild_person_index(
    tx: &mut Transaction<'_, Postgres>,
    company: &CompanyComparison,
    facilities: &[FacilityComparison],
    people_by_run_id: &HashMap<String, Vec<ExtractedPerson>>,
) -> Result<usize, ApplyError> {
    let mut run_names: HashMap<String, String> = sqlx::query_as::<_, (String, String)>(
        "SELECT ps_run_id, run_name FROM clients.ps_sync_state \
         WHERE workflow = 'intake' AND ps_run_id = ANY($1)",
    )
    .bind(people_by_run_id.keys().cloned().collect::<Vec<_>>())
    .fetch_all(&mut **tx)
    .await
    .unwrap_or_default()
    .into_iter()
    .collect();

    if let Some(run_id) = &company.row.ps_intake_run_id {
        run_names
            .entry(run_id.clone())
            .or_insert_with(|| company.row.legal_name.clone());
    }
    for facility in facilities {
        if let Some(run_id) = &facility.row.ps_intake_run_id {
            run_names
                .entry(run_id.clone())
                .or_insert_with(|| facility.row.name.clone());
        }
    }

    let refreshed_run_ids: Vec<String> = people_by_run_id.keys().cloned().collect();

    sqlx::query(
        "DELETE FROM clients.ps_person_index WHERE workflow = 'intake' AND ps_run_id = ANY($1)",
    )
    .bind(&refreshed_run_ids)
    .execute(&mut **tx)
    .await
    .map_err(|error| ApplyError::Database {
        step: "clear ps_person_index",
        error,
    })?;

    let mut index_run_ids: Vec<&str> = Vec::new();
    let mut index_run_names: Vec<&str> = Vec::new();
    let mut index_full_names: Vec<&str> = Vec::new();
    let mut index_emails: Vec<Option<&str>> = Vec::new();
    let mut index_phones: Vec<Option<&str>> = Vec::new();
    let mut index_roles: Vec<&str> = Vec::new();
    for (run_id, people) in people_by_run_id {
        let run_name = run_names
            .get(run_id)
            .map(String::as_str)
            .unwrap_or(run_id.as_str());

        for person in people {
            index_run_ids.push(run_id);
            index_run_names.push(run_name);
            index_full_names.push(&person.full_name);
            index_emails.push(person.email.as_deref());
            index_phones.push(person.phone.as_deref());
            index_roles.push(person.role);
        }
    }

    let people_indexed = index_run_ids.len();
    if people_indexed > 0 {
        sqlx::query(
            "INSERT INTO clients.ps_person_index
                 (workflow, ps_run_id, run_name, full_name, email, phone, role)
             SELECT 'intake', run_id, run_name, full_name, email, phone, role
               FROM UNNEST($1::text[], $2::text[], $3::text[], $4::text[], $5::text[], $6::text[])
                    AS t(run_id, run_name, full_name, email, phone, role)",
        )
        .bind(&index_run_ids)
        .bind(&index_run_names)
        .bind(&index_full_names)
        .bind(&index_emails)
        .bind(&index_phones)
        .bind(&index_roles)
        .execute(&mut **tx)
        .await
        .map_err(|error| ApplyError::Database {
            step: "index people",
            error,
        })?;
    }

    Ok(people_indexed)
}

/// Refresh every linked facility's Elavon/Merchant Account picture --
/// see `MerchantAccountRefresh`'s own doc comment for why this button
/// never touched this data before. Always a full overwrite (no
/// manual-edit protection on this tab, same as `resync_elavon_data`),
/// so unlike the company/facility steps there's no protected-field
/// comparison first. Returns how many facilities it refreshed.
pub(super) async fn refresh_merchant_accounts(
    tx: &mut Transaction<'_, Postgres>,
    refreshes: &HashMap<Uuid, MerchantAccountRefresh>,
) -> Result<usize, ApplyError> {
    let mut refreshed = 0;

    for (facility_id, refresh) in refreshes {
        resync_merchant_account_run(
            tx,
            *facility_id,
            &refresh.mapped,
            &refresh.ps_new_merchant_run_id,
            refresh.credentials_added_to_qms,
        )
        .await
        .map_err(|error| ApplyError::MerchantAccount {
            facility_id: *facility_id,
            error,
        })?;

        upsert_task_status(tx, *facility_id, "merchant_account", &refresh.tasks)
            .await
            .map_err(|error| ApplyError::MerchantTasks {
                facility_id: *facility_id,
                error,
            })?;

        refreshed += 1;
    }

    Ok(refreshed)
}

/// Runs every write step in order inside the caller's transaction.
pub(super) async fn write_all(
    tx: &mut Transaction<'_, Postgres>,
    company: &CompanyComparison,
    facilities: &[FacilityComparison],
    people_by_run_id: &HashMap<String, Vec<ExtractedPerson>>,
    merchant_account_refreshes: &HashMap<Uuid, MerchantAccountRefresh>,
    resolutions: &[ConflictResolution],
) -> Result<Written, ApplyError> {
    let company_updated = update_company(tx, company, resolutions).await?;
    let facilities_updated = update_facilities(tx, facilities, resolutions).await?;
    let people_indexed = rebuild_person_index(tx, company, facilities, people_by_run_id).await?;
    let merchant_accounts_refreshed =
        refresh_merchant_accounts(tx, merchant_account_refreshes).await?;

    Ok(Written {
        updated_count: usize::from(company_updated) + facilities_updated,
        people_indexed,
        merchant_accounts_refreshed,
    })
}
