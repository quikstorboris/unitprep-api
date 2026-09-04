//! The real "Add to OO" trigger for Phase 3's confirmation screen --
//! given one Intake run designated the company source and zero or more
//! Intake runs designated facilities, creates one real
//! `clients.companies` row and one `clients.facilities` row per
//! facility run, all attached to that same company.
//!
//! **The company source run may also be one of the facility runs**
//! (Boris, 2026-09-02, reversing the original 2026-08-31 either/or
//! design): a company's corporate data usually comes from whichever
//! facility's Intake run answered "first time = Yes" (see
//! `clients.companies.ps_intake_run_id`'s own migration comment) -- that
//! same run is still a real, separate physical facility (Prairie
//! Enterprises' own Highway 20, confirmed against live data) and needs
//! its own `clients.facilities` row too, not a choice between the two.
//! Nothing here rejects `company_intake_run_id` also appearing in
//! `facility_selections`; the resulting company and facility rows simply
//! both cite it as their own `ps_intake_run_id` for provenance -- no
//! schema constraint ties the two together, so this needed no migration.
//!
//! **Takes the confirmation screen's *reviewed* values, not a fresh
//! PS-derived guess** (2026-09-01): `reviewed_company`/each facility's
//! `EditableFacilityFields` come from `api::clients_preview`'s output,
//! possibly hand-edited by whoever ran the import. This module still
//! re-fetches each run's raw fields itself -- `raw_ps_snapshot` and the
//! facility-policies/people extraction (fees, taxes, delinquency steps,
//! owners/DMs/managers) were never shown on the confirmation screen, so
//! they always come from a fresh, authoritative mapping. Only the
//! reviewed company/facility fields are allowed to override that fresh
//! mapping -- `apply_facility_overrides` merges field-by-field rather
//! than replacing the struct wholesale, specifically so a field the
//! confirmation screen never showed (Go Live Date) can never be
//! clobbered by whatever a stale/incomplete client payload happens to
//! send.
//!
//! **Adding a facility to a company that already exists in OO** (not
//! part of this batch) isn't handled here -- per Boris's own call,
//! that's a separate search pulling the facility in on its own, not a
//! feature of this endpoint. `company_intake_run_id` is always required;
//! this always creates a new company.
//!
//! **Every distinct run id is fetched from PS exactly once, concurrently**
//! (2026-09-02): before this, the company's own run was fetched, then
//! every facility's run was fetched again in a sequential `for` loop --
//! confirmed against a real Create (18.1s for 1 company + 3 facilities,
//! matching 4 sequential live fetches). Now that the company's source
//! run can also be one of the facility runs (this module's own doc
//! comment above), a naive per-facility loop would even double-fetch
//! that one run's fields. Every distinct run id across
//! `company_intake_run_id` and `facility_selections` is fetched once, in
//! one concurrent batch (same `join_all` pattern `api::clients_preview`
//! already uses), then the DB writes replay sequentially against the
//! shared transaction -- a `Transaction` only ever allows one write in
//! flight at a time, so only the network I/O benefits from concurrency.

use std::collections::{HashMap, HashSet};

use futures::future::join_all;
use serde::Deserialize;
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::clients::intake_mapping::{map_intake_fields, MappedCompany, MappedFacility};
use crate::clients::merchant_account_mapping::{credentials_added_to_qms_from_tasks, map_merchant_account_fields};
use crate::clients::people::PersonAssignment;
use crate::clients::repository::{
    ingest_merchant_account_run, insert_company, insert_facility, insert_facility_policies_and_people,
};
use crate::process_street::{FormField, ProcessStreetClient, ProcessStreetError, Task};

/// The subset of `MappedFacility` the confirmation screen actually
/// shows and lets a manager edit -- everything except `go_live_date`
/// ("Original Go Live Date"), which is displayed but deliberately
/// non-editable (see the vault: an "Updated Go Live Date" sourced from
/// ClickUp is the real future editable field, not this one). Excluding
/// it here, rather than just not rendering an input for it, makes
/// clobbering it structurally impossible, not just a UI convention.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EditableFacilityFields {
    pub name: Option<String>,
    pub street_address: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub zip: Option<String>,
    pub phone: Option<String>,
    pub email: Option<String>,
    pub units_count: Option<i32>,
    pub primary_storage_offering: Option<String>,
    pub previous_pms: Option<String>,
    pub access_control_system: Option<String>,
    pub dropbox_folder_url: Option<String>,
    pub subdomain: Option<String>,
    pub subdomain_exists_in_qms_raw: Option<String>,
    pub system_email: Option<String>,
    pub website_url: Option<String>,
    /// This facility's own reviewed People roster from the confirmation
    /// screen -- who actually gets linked via `link_person_to_facility`,
    /// **not** whatever `map_intake_fields` re-derives from this run's
    /// own raw owner/DM/manager text. See `PersonAssignment`'s own doc
    /// comment for why facility-level attribution has to be a human
    /// call. Defaults to empty, not "keep whatever was there" -- same
    /// full-resubmit convention every other field on this struct already
    /// follows (see `apply_facility_overrides`'s own doc comment).
    #[serde(default)]
    pub people: Vec<PersonAssignment>,
}

/// Field names in `MappedCompany` that differ between a fresh Intake
/// mapping and the confirmation screen's reviewed values -- recorded as
/// `clients.companies.manually_edited_fields` (see this migration's own
/// comment) so a later sync -- scheduled or the scoped Re-sync button --
/// never silently overwrites a real human correction with whatever
/// Process Street currently says for that one field.
pub(crate) fn diff_company_fields(fresh: &MappedCompany, reviewed: &MappedCompany) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if fresh.legal_name != reviewed.legal_name {
        changed.push("legal_name");
    }
    if fresh.corporate_email != reviewed.corporate_email {
        changed.push("corporate_email");
    }
    if fresh.corporate_phone != reviewed.corporate_phone {
        changed.push("corporate_phone");
    }
    if fresh.corporate_address_street != reviewed.corporate_address_street {
        changed.push("corporate_address_street");
    }
    if fresh.corporate_address_city != reviewed.corporate_address_city {
        changed.push("corporate_address_city");
    }
    if fresh.corporate_address_state != reviewed.corporate_address_state {
        changed.push("corporate_address_state");
    }
    if fresh.corporate_address_zip != reviewed.corporate_address_zip {
        changed.push("corporate_address_zip");
    }
    if fresh.subdomain != reviewed.subdomain {
        changed.push("subdomain");
    }
    if fresh.accepted_payment_methods != reviewed.accepted_payment_methods {
        changed.push("accepted_payment_methods");
    }
    if fresh.accounting_basis != reviewed.accounting_basis {
        changed.push("accounting_basis");
    }
    if fresh.payment_scheme != reviewed.payment_scheme {
        changed.push("payment_scheme");
    }
    if fresh.offers_tenant_insurance_raw != reviewed.offers_tenant_insurance_raw {
        changed.push("offers_tenant_insurance_raw");
    }
    if fresh.insurance_provider != reviewed.insurance_provider {
        changed.push("insurance_provider");
    }
    if fresh.website_url != reviewed.website_url {
        changed.push("website_url");
    }
    changed
}

/// `diff_company_fields`'s counterpart for `EditableFacilityFields` --
/// `go_live_date` is never compared since it's not part of that type at
/// all (see this module's own doc comment on why it's structurally
/// excluded from review).
fn diff_facility_fields(fresh: &MappedFacility, reviewed: &EditableFacilityFields) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if fresh.name != reviewed.name {
        changed.push("name");
    }
    if fresh.street_address != reviewed.street_address {
        changed.push("street_address");
    }
    if fresh.city != reviewed.city {
        changed.push("city");
    }
    if fresh.state != reviewed.state {
        changed.push("state");
    }
    if fresh.zip != reviewed.zip {
        changed.push("zip");
    }
    if fresh.phone != reviewed.phone {
        changed.push("phone");
    }
    if fresh.email != reviewed.email {
        changed.push("email");
    }
    if fresh.units_count != reviewed.units_count {
        changed.push("units_count");
    }
    if fresh.primary_storage_offering != reviewed.primary_storage_offering {
        changed.push("primary_storage_offering");
    }
    if fresh.previous_pms != reviewed.previous_pms {
        changed.push("previous_pms");
    }
    if fresh.access_control_system != reviewed.access_control_system {
        changed.push("access_control_system");
    }
    if fresh.dropbox_folder_url != reviewed.dropbox_folder_url {
        changed.push("dropbox_folder_url");
    }
    if fresh.subdomain != reviewed.subdomain {
        changed.push("subdomain");
    }
    if fresh.subdomain_exists_in_qms_raw != reviewed.subdomain_exists_in_qms_raw {
        changed.push("subdomain_exists_in_qms_raw");
    }
    if fresh.system_email != reviewed.system_email {
        changed.push("system_email");
    }
    if fresh.website_url != reviewed.website_url {
        changed.push("website_url");
    }
    changed
}

/// Overlays the reviewed fields onto a freshly-mapped `MappedFacility`
/// -- `go_live_date` (and anything else future fields might add here
/// before this module catches up) is left exactly as PS mapped it,
/// never touched by the override.
fn apply_facility_overrides(mapped: MappedFacility, overrides: EditableFacilityFields) -> MappedFacility {
    MappedFacility {
        name: overrides.name,
        street_address: overrides.street_address,
        city: overrides.city,
        state: overrides.state,
        zip: overrides.zip,
        phone: overrides.phone,
        email: overrides.email,
        units_count: overrides.units_count,
        primary_storage_offering: overrides.primary_storage_offering,
        previous_pms: overrides.previous_pms,
        access_control_system: overrides.access_control_system,
        dropbox_folder_url: overrides.dropbox_folder_url,
        subdomain: overrides.subdomain,
        subdomain_exists_in_qms_raw: overrides.subdomain_exists_in_qms_raw,
        system_email: overrides.system_email,
        website_url: overrides.website_url,
        go_live_date: mapped.go_live_date,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("Process Street request failed: {0}")]
    ProcessStreet(#[from] ProcessStreetError),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    IngestMerchantAccount(#[from] crate::clients::repository::IngestMerchantAccountError),
    /// One or more of the selected Intake run ids already has a
    /// `clients.companies`/`clients.facilities` row -- carries every
    /// offending run id so the caller can report exactly which
    /// selection(s) need to be unchecked, not just "something's already
    /// imported."
    #[error("already imported: {0:?}")]
    AlreadyImported(Vec<String>),
}

#[derive(Debug)]
pub struct CreatedFromSelection {
    pub company_id: Uuid,
    pub facility_ids: Vec<Uuid>,
}

/// Checks every involved run id against both `clients.facilities` and
/// `clients.companies` before anything is fetched from PS or written --
/// a duplicate must block the whole batch, not partially import it.
/// `pub(crate)` so `api::clients_create` can run this as its own quick
/// fail-fast check, in its own short transaction, before ever calling
/// out to Process Street (see this module's own doc comment on why the
/// production handler doesn't use `create_company_and_facilities`
/// directly). `write_create_data` below runs it again anyway,
/// atomically with the actual writes -- redundant against the common
/// case, but closes the race window between the fail-fast check and the
/// write for the two requests that lose it.
pub(crate) async fn check_not_already_imported(
    tx: &mut Transaction<'_, Postgres>,
    run_ids: &[&str],
) -> Result<(), CreateError> {
    let already: Vec<(String,)> = sqlx::query_as(
        "SELECT ps_intake_run_id FROM clients.facilities WHERE ps_intake_run_id = ANY($1)
         UNION
         SELECT ps_intake_run_id FROM clients.companies WHERE ps_intake_run_id = ANY($1)",
    )
    .bind(run_ids)
    .fetch_all(&mut **tx)
    .await?;

    if already.is_empty() {
        Ok(())
    } else {
        Err(CreateError::AlreadyImported(already.into_iter().map(|(id,)| id).collect()))
    }
}

/// Creates a company from `company_intake_run_id` (using the reviewed
/// `reviewed_company` fields for its own row) and a facility for each
/// entry in `facility_selections` (`(run_id, reviewed fields,
/// merchant_account_run_id)`), all attached to that company. Takes an
/// already-open transaction -- the caller decides whether to commit or
/// roll back, same discipline every other write in this domain uses
/// (see `clients::ingest::ingest_facility`).
///
/// **A facility's own Merchant Account data is only ever ingested here**
/// (2026-09-03 fix): the correlation itself was already resolved by
/// `api::clients_preview` (auto or the user's own explicit
/// "Potential Duplicates" pick), but nothing carried that resolution
/// through to this, the actual write step -- `merchant_account_run_id`
/// was computed, shown, and then silently dropped, so
/// `clients.facility_merchant_accounts`/`facility_merchant_account_parties`
/// never got a single row written by this endpoint despite the
/// confirmation screen visibly resolving Elavon data for real facilities.
/// `Some(ma_run_id)` here means: fetch that run too (folded into the
/// same combined concurrent batch below, not a second round of fetches)
/// and ingest it via `repository::ingest_merchant_account_run` right
/// after that facility's own row is created.
///
/// **Test-only as of 2026-09-03** (`#[cfg(test)]`) -- `api::clients_create`'s
/// real handler no longer calls this; it calls `fetch_create_data`/
/// `write_create_data` separately instead, so its own transaction never
/// spans the live Process Street fetch (see `fetch_create_data`'s own
/// doc comment). Kept for this module's live tests, which still want
/// "one call, one transaction, roll back at the end" so a repeated test
/// run never leaves real rows in the shared dev database.
#[cfg(test)]
pub async fn create_company_and_facilities(
    client: &ProcessStreetClient,
    tx: &mut Transaction<'_, Postgres>,
    company_intake_run_id: &str,
    reviewed_company: &MappedCompany,
    facility_selections: &[(String, EditableFacilityFields, Option<String>)],
) -> Result<CreatedFromSelection, CreateError> {
    let mut all_run_ids: Vec<&str> = vec![company_intake_run_id];
    all_run_ids.extend(facility_selections.iter().map(|(run_id, _, _)| run_id.as_str()));
    check_not_already_imported(tx, &all_run_ids).await?;

    let fetched = fetch_create_data(client, company_intake_run_id, facility_selections).await?;
    write_create_data(tx, company_intake_run_id, reviewed_company, facility_selections, &fetched).await
}

/// Every distinct run id's fields/tasks, fetched once from PS,
/// concurrently, and nothing else -- no database access at all. Split
/// out from `create_company_and_facilities` 2026-09-03: that combined
/// function holds its caller's transaction open for its entire
/// duration, including this fetch, which is a live external network
/// round trip PS itself controls the timing of -- exactly the same
/// "transaction held open across a slow external call" shape that
/// leaked a stuck `idle in transaction` connection on the Elavon tab's
/// own link action (see `api::clients_elavon`'s matching fix). `
/// api::clients_create`'s real handler calls this and `write_create_data`
/// below separately, opening a transaction only around the write; the
/// combined function above still exists, still holding its caller's `tx`
/// across both halves, purely so this module's own live tests can keep
/// wrapping a whole create-and-verify-then-rollback cycle in one
/// transaction without leaving real rows in the shared dev database.
pub struct FetchedCreateData {
    fields_by_run_id: HashMap<String, Vec<FormField>>,
    tasks_by_ma_run_id: HashMap<String, Vec<Task>>,
}

pub async fn fetch_create_data(
    client: &ProcessStreetClient,
    company_intake_run_id: &str,
    facility_selections: &[(String, EditableFacilityFields, Option<String>)],
) -> Result<FetchedCreateData, CreateError> {
    // Every distinct run id fetched once, concurrently -- see this
    // module's own doc comment. Deduped via the HashSet since the
    // company's source run is now often also one of the facility runs.
    // Every resolved Merchant Account run joins the same batch -- one
    // combined concurrent fetch, not a second round after facility
    // creation.
    let mut fetch_run_ids: HashSet<&str> = std::iter::once(company_intake_run_id)
        .chain(facility_selections.iter().map(|(run_id, _, _)| run_id.as_str()))
        .collect();
    fetch_run_ids.extend(
        facility_selections
            .iter()
            .filter_map(|(_, _, ma_run_id)| ma_run_id.as_deref()),
    );
    let fetches = fetch_run_ids.into_iter().map(|run_id| async move {
        let result = client.get_run_form_fields(run_id).await;
        (run_id.to_string(), result)
    });
    let mut fields_by_run_id: HashMap<String, Vec<FormField>> = HashMap::new();
    for (run_id, result) in join_all(fetches).await {
        fields_by_run_id.insert(run_id, result?);
    }

    // A second small concurrent batch, tasks (not form fields) for every
    // distinct Merchant Account run only -- `credentials_added_to_qms`
    // is a checklist task's completion, not a form answer (see
    // `merchant_account_mapping::credentials_added_to_qms_from_tasks`'s
    // own doc for why). Intake/Contract Order tasks aren't fetched here;
    // `ps_task_status` population for those is separate, still-unbuilt
    // work (Phase 4 item 7).
    let distinct_ma_run_ids: HashSet<&str> =
        facility_selections.iter().filter_map(|(_, _, ma_run_id)| ma_run_id.as_deref()).collect();
    let task_fetches = distinct_ma_run_ids.into_iter().map(|run_id| async move {
        let result = client.get_run_tasks(run_id).await;
        (run_id.to_string(), result)
    });
    let mut tasks_by_ma_run_id: HashMap<String, Vec<Task>> = HashMap::new();
    for (run_id, result) in join_all(task_fetches).await {
        tasks_by_ma_run_id.insert(run_id, result?);
    }

    Ok(FetchedCreateData { fields_by_run_id, tasks_by_ma_run_id })
}

/// The database-only half -- takes data `fetch_create_data` already
/// fetched, and an already-open transaction, and does every write. No
/// network access, so nothing here holds `tx` open longer than real
/// local database work needs. Runs its own `check_not_already_imported`
/// first (redundant with the caller's own fail-fast check when there
/// was one, but this is the one that's actually atomic with the write --
/// see that function's own doc comment).
pub async fn write_create_data(
    tx: &mut Transaction<'_, Postgres>,
    company_intake_run_id: &str,
    reviewed_company: &MappedCompany,
    facility_selections: &[(String, EditableFacilityFields, Option<String>)],
    fetched: &FetchedCreateData,
) -> Result<CreatedFromSelection, CreateError> {
    let mut all_run_ids: Vec<&str> = vec![company_intake_run_id];
    all_run_ids.extend(facility_selections.iter().map(|(run_id, _, _)| run_id.as_str()));
    check_not_already_imported(tx, &all_run_ids).await?;

    let company_fields = fetched
        .fields_by_run_id
        .get(company_intake_run_id)
        .expect("the company's own run was fetched above");
    let company_snapshot: Value = serde_json::to_value(company_fields).unwrap_or(Value::Null);
    let fresh_company_mapping = map_intake_fields(company_fields);
    let company_manually_edited_fields = diff_company_fields(&fresh_company_mapping.company, reviewed_company);

    let legal_name = reviewed_company
        .legal_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("(unnamed company)");

    let company_id = insert_company(
        tx,
        legal_name,
        reviewed_company,
        company_intake_run_id,
        &company_snapshot,
        &company_manually_edited_fields,
    )
    .await?;

    let mut facility_ids = Vec::with_capacity(facility_selections.len());
    for (run_id, overrides, merchant_account_run_id) in facility_selections {
        let fields = fetched
            .fields_by_run_id
            .get(run_id.as_str())
            .expect("every facility's own run was fetched above");
        let mut mapped = map_intake_fields(fields);
        let facility_manually_edited_fields = diff_facility_fields(&mapped.facility, overrides);
        mapped.facility = apply_facility_overrides(mapped.facility, overrides.clone());
        let snapshot: Value = serde_json::to_value(fields).unwrap_or(Value::Null);

        let facility_id = insert_facility(
            tx,
            company_id,
            &mapped.facility,
            run_id,
            &snapshot,
            &facility_manually_edited_fields,
        )
        .await?;
        insert_facility_policies_and_people(tx, facility_id, &mapped, &snapshot, &overrides.people).await?;

        if let Some(ma_run_id) = merchant_account_run_id {
            let ma_fields = fetched
                .fields_by_run_id
                .get(ma_run_id.as_str())
                .expect("every resolved Merchant Account run was fetched above");
            let mapped_ma = map_merchant_account_fields(ma_fields);
            let ma_tasks = fetched
                .tasks_by_ma_run_id
                .get(ma_run_id.as_str())
                .expect("every resolved Merchant Account run's tasks were fetched above");
            let credentials_added_to_qms = credentials_added_to_qms_from_tasks(ma_tasks);
            ingest_merchant_account_run(tx, facility_id, &mapped_ma, ma_run_id, credentials_added_to_qms).await?;
        }

        facility_ids.push(facility_id);
    }

    Ok(CreatedFromSelection { company_id, facility_ids })
}

#[cfg(test)]
mod override_tests {
    use super::*;

    pub(super) fn fully_mapped_facility() -> MappedFacility {
        MappedFacility {
            name: Some("PS Original Name".to_string()),
            street_address: Some("PS Original Street".to_string()),
            city: Some("PS Original City".to_string()),
            state: Some("PS Original State".to_string()),
            zip: Some("00000".to_string()),
            phone: Some("000-000-0000".to_string()),
            email: Some("original@example.com".to_string()),
            units_count: Some(100),
            primary_storage_offering: Some("PS Original Offering".to_string()),
            previous_pms: Some("PS Original PMS".to_string()),
            access_control_system: Some("PS Original Access".to_string()),
            go_live_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
            dropbox_folder_url: Some("https://original.example.com".to_string()),
            subdomain: Some("original".to_string()),
            subdomain_exists_in_qms_raw: Some("No".to_string()),
            system_email: Some("system-original@example.com".to_string()),
            website_url: Some("https://original.example.com".to_string()),
        }
    }

    #[test]
    fn an_edited_field_overrides_the_freshly_mapped_value() {
        let overrides = EditableFacilityFields {
            name: Some("Corrected Name".to_string()),
            ..Default::default()
        };

        let result = apply_facility_overrides(fully_mapped_facility(), overrides);

        assert_eq!(result.name.as_deref(), Some("Corrected Name"));
    }

    #[test]
    fn go_live_date_is_never_touched_by_an_override_regardless_of_what_it_carries() {
        // EditableFacilityFields has no go_live_date field at all -- this
        // proves the merge preserves the freshly-mapped value rather than
        // e.g. defaulting it to None.
        let mapped = fully_mapped_facility();
        let original_go_live_date = mapped.go_live_date;

        let result = apply_facility_overrides(mapped, EditableFacilityFields::default());

        assert_eq!(result.go_live_date, original_go_live_date);
    }

    #[test]
    fn an_untouched_field_falls_back_to_none_not_the_original_mapped_value() {
        // Overrides always come from the confirmation screen's full
        // reviewed state (every editable field always resubmitted), so a
        // `None` here means "the user cleared this field", not "leave it
        // alone" -- confirms the merge is a real overlay of every
        // editable field, not a sparse patch.
        let result = apply_facility_overrides(fully_mapped_facility(), EditableFacilityFields::default());

        assert_eq!(result.name, None);
        assert_eq!(result.street_address, None);
    }
}

#[cfg(test)]
mod diff_tests {
    use super::*;
    use super::override_tests::fully_mapped_facility;

    fn fully_mapped_company() -> MappedCompany {
        MappedCompany {
            legal_name: Some("Prairie Enterprises LLC".to_string()),
            corporate_email: Some("office@prairie-enterprises.com".to_string()),
            corporate_phone: Some("815-568-1307".to_string()),
            corporate_address_street: Some("1030 East Grant Highway".to_string()),
            corporate_address_city: Some("Marengo".to_string()),
            corporate_address_state: Some("IL".to_string()),
            corporate_address_zip: Some("60152".to_string()),
            subdomain: Some("prairie-enterprises.qms-email.com".to_string()),
            accepted_payment_methods: Some("Credit Card, ACH".to_string()),
            accounting_basis: Some("Cash".to_string()),
            payment_scheme: Some("Advance".to_string()),
            offers_tenant_insurance_raw: Some("Yes".to_string()),
            insurance_provider: Some("Example Insurance Co".to_string()),
            website_url: Some("https://prairie-enterprises.com".to_string()),
        }
    }

    fn fully_mapped_facility_fields() -> EditableFacilityFields {
        let mapped = fully_mapped_facility();
        EditableFacilityFields {
            name: mapped.name,
            street_address: mapped.street_address,
            city: mapped.city,
            state: mapped.state,
            zip: mapped.zip,
            phone: mapped.phone,
            email: mapped.email,
            units_count: mapped.units_count,
            primary_storage_offering: mapped.primary_storage_offering,
            previous_pms: mapped.previous_pms,
            access_control_system: mapped.access_control_system,
            dropbox_folder_url: mapped.dropbox_folder_url,
            subdomain: mapped.subdomain,
            subdomain_exists_in_qms_raw: mapped.subdomain_exists_in_qms_raw,
            system_email: mapped.system_email,
            website_url: mapped.website_url,
            people: Vec::new(),
        }
    }

    #[test]
    fn identical_company_values_produce_no_diff() {
        let fresh = fully_mapped_company();
        let reviewed = fresh.clone();

        assert!(diff_company_fields(&fresh, &reviewed).is_empty());
    }

    #[test]
    fn a_corrected_company_field_is_named_in_the_diff() {
        let fresh = fully_mapped_company();
        let reviewed = MappedCompany {
            legal_name: Some("Corrected Legal Name LLC".to_string()),
            ..fresh.clone()
        };

        assert_eq!(diff_company_fields(&fresh, &reviewed), vec!["legal_name"]);
    }

    #[test]
    fn multiple_corrected_company_fields_are_all_named() {
        let fresh = fully_mapped_company();
        let reviewed = MappedCompany {
            legal_name: Some("Corrected Legal Name LLC".to_string()),
            corporate_phone: Some("555-000-0000".to_string()),
            ..fresh.clone()
        };

        assert_eq!(
            diff_company_fields(&fresh, &reviewed),
            vec!["legal_name", "corporate_phone"]
        );
    }

    #[test]
    fn identical_facility_values_produce_no_diff() {
        let fresh = fully_mapped_facility();
        let reviewed = fully_mapped_facility_fields();

        assert!(diff_facility_fields(&fresh, &reviewed).is_empty());
    }

    #[test]
    fn a_corrected_facility_field_is_named_in_the_diff() {
        let fresh = fully_mapped_facility();
        let reviewed = EditableFacilityFields {
            name: Some("Corrected Facility Name".to_string()),
            ..fully_mapped_facility_fields()
        };

        assert_eq!(diff_facility_fields(&fresh, &reviewed), vec!["name"]);
    }

    /// go_live_date isn't part of `EditableFacilityFields` at all, so it
    /// can never show up in a facility diff -- confirms the diff can't
    /// even be asked about the one field that's structurally excluded
    /// from review.
    #[test]
    fn go_live_date_never_appears_in_a_facility_diff() {
        let fresh = fully_mapped_facility();
        let reviewed = fully_mapped_facility_fields();

        let changed = diff_facility_fields(&fresh, &reviewed);

        assert!(!changed.contains(&"go_live_date"));
    }
}

#[cfg(test)]
mod live_tests {
    use serial_test::serial;

    use crate::process_street::{ProcessStreetClient, ProcessStreetConfig};

    use super::*;

    fn set_test_key() {
        std::env::set_var(
            "CLIENT_PII_ENCRYPTION_KEY",
            "4444444444444444444444444444444444444444444444444444444444444444",
        );
    }
    fn clear_test_key() {
        std::env::remove_var("CLIENT_PII_ENCRYPTION_KEY");
    }

    /// Proves the split-creation flow against the real live PS API and
    /// real Postgres: Highway 20 marked as the Company source, zero
    /// facility selections in this run -- still a valid, minimal case
    /// (e.g. a client with no other facilities to import yet), distinct
    /// from `the_companys_source_run_also_becomes_its_own_facility`
    /// below. Rolled back so nothing persists.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres AND a real Process Street API key -- see doc comment"]
    #[serial(client_pii_encryption_key_env)]
    async fn creates_a_company_with_no_facilities_from_a_real_intake_run() {
        let _ = dotenvy::from_filename(".env.local");
        set_test_key();

        let ps_config =
            ProcessStreetConfig::from_env().expect("PROCESS_STREET_API_KEY must be set in .env.local");
        let client = ProcessStreetClient::new(ps_config);

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let user_id = Uuid::new_v4();
        let mut tx = crate::auth::begin_rls_transaction(&db, user_id, &["onboarding_manager".to_string()])
            .await
            .expect("beginning an RLS transaction must succeed");

        let reviewed_company = MappedCompany {
            legal_name: Some("Prairie Enterprises LLC".to_string()),
            ..Default::default()
        };
        let result = create_company_and_facilities(
            &client,
            &mut tx,
            "iy22NyiqGjwAAytKp0NErQ", // Highway 20 Intake/Progress
            &reviewed_company,
            &[],
        )
        .await
        .expect("creating a company from the real live run must succeed");

        assert!(result.facility_ids.is_empty());

        let (legal_name,): (String,) =
            sqlx::query_as("SELECT legal_name FROM clients.companies WHERE id = $1")
                .bind(result.company_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(legal_name, "Prairie Enterprises LLC");

        // The company's own run must NOT also have created a facility --
        // exactly zero facilities point at this company.
        let (facility_count,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM clients.facilities WHERE company_id = $1")
                .bind(result.company_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(facility_count, 0);

        tx.rollback()
            .await
            .expect("rollback must succeed -- this is a one-time check, not a real import");
        clear_test_key();
    }

    /// Regression test for Boris's real case (2026-09-02): Highway 20 is
    /// both the company source (it's Prairie Enterprises' "first time"
    /// facility, with real Corporate Info) *and* a genuine, separate
    /// physical facility -- selecting it as `company_intake_run_id` must
    /// not exclude it from also appearing in `facility_selections`.
    /// Rolled back so nothing persists.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres AND a real Process Street API key -- see doc comment"]
    #[serial(client_pii_encryption_key_env)]
    async fn the_companys_source_run_also_becomes_its_own_facility() {
        let _ = dotenvy::from_filename(".env.local");
        set_test_key();

        let ps_config =
            ProcessStreetConfig::from_env().expect("PROCESS_STREET_API_KEY must be set in .env.local");
        let client = ProcessStreetClient::new(ps_config);

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let user_id = Uuid::new_v4();
        let mut tx = crate::auth::begin_rls_transaction(&db, user_id, &["onboarding_manager".to_string()])
            .await
            .expect("beginning an RLS transaction must succeed");

        let reviewed_company = MappedCompany {
            legal_name: Some("Prairie Enterprises LLC".to_string()),
            ..Default::default()
        };
        let highway_20_run_id = "iy22NyiqGjwAAytKp0NErQ"; // Highway 20 Intake/Progress
        let result = create_company_and_facilities(
            &client,
            &mut tx,
            highway_20_run_id,
            &reviewed_company,
            &[(highway_20_run_id.to_string(), EditableFacilityFields::default(), None)],
        )
        .await
        .expect("creating a company whose source run is also a facility must succeed");

        assert_eq!(result.facility_ids.len(), 1);

        let (facility_ps_intake_run_id,): (Option<String>,) =
            sqlx::query_as("SELECT ps_intake_run_id FROM clients.facilities WHERE id = $1")
                .bind(result.facility_ids[0])
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(facility_ps_intake_run_id.as_deref(), Some(highway_20_run_id));

        tx.rollback()
            .await
            .expect("rollback must succeed -- this is a one-time check, not a real import");
        clear_test_key();
    }

    /// Duplicate-import protection: attempting to create a company from
    /// a run id that (within this same uncommitted transaction) has
    /// already been used must fail with `AlreadyImported`, not silently
    /// create a second company for the same real business.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres AND a real Process Street API key -- see doc comment"]
    #[serial(client_pii_encryption_key_env)]
    async fn refuses_to_recreate_an_already_imported_run() {
        let _ = dotenvy::from_filename(".env.local");
        set_test_key();

        let ps_config =
            ProcessStreetConfig::from_env().expect("PROCESS_STREET_API_KEY must be set in .env.local");
        let client = ProcessStreetClient::new(ps_config);

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let user_id = Uuid::new_v4();
        let mut tx = crate::auth::begin_rls_transaction(&db, user_id, &["onboarding_manager".to_string()])
            .await
            .expect("beginning an RLS transaction must succeed");

        let reviewed_company = MappedCompany {
            legal_name: Some("Prairie Enterprises LLC".to_string()),
            ..Default::default()
        };
        create_company_and_facilities(&client, &mut tx, "iy22NyiqGjwAAytKp0NErQ", &reviewed_company, &[])
            .await
            .expect("first create must succeed");

        let second_attempt =
            create_company_and_facilities(&client, &mut tx, "iy22NyiqGjwAAytKp0NErQ", &reviewed_company, &[])
                .await;

        match second_attempt {
            Err(CreateError::AlreadyImported(ids)) => {
                assert_eq!(ids, vec!["iy22NyiqGjwAAytKp0NErQ".to_string()]);
            }
            other => panic!("expected AlreadyImported, got {other:?}"),
        }

        tx.rollback()
            .await
            .expect("rollback must succeed -- this is a one-time check, not a real import");
        clear_test_key();
    }
}
