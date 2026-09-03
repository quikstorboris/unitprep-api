//! Delta-aware background sync feeding `clients.ps_person_index` --
//! Phase 2's harder half (person-name search). Wired into `main.rs`:
//! `start_background_sync_task` runs whenever `PROCESS_STREET_API_KEY`
//! is configured, alongside `api::clients_search`, which reads what this
//! writes. Also proven directly against the real API and real Postgres
//! by `live_tests::sync_one_run_indexes_a_real_run_and_skips_an_unchanged_one`,
//! the same "prove it, then roll back" discipline `clients::ingest`'s
//! own live test uses.
//!
//! **The delta mechanism**: `list_workflow_runs` is cheap (one paginated
//! list call, no per-run fetch) and every run PS returns carries its own
//! `audit.updatedDate` for free. A run's `form-fields` -- the expensive
//! per-run fetch this module exists to avoid doing unnecessarily -- is
//! only re-fetched when that timestamp has moved past what
//! `clients.ps_sync_state` last recorded for it. `updatedDate` only
//! changes when someone actually edits that run in PS, so a facility
//! whose Intake run nobody has touched since the last sync costs
//! nothing beyond the one shared list call.
//!
//! **RLS**: this task has no real authenticated caller (it runs on a
//! timer, not behind a request) -- same situation
//! `client_ops::vendor_format::start_refresh_task` already solves. Its
//! own `SYSTEM_USER_ID` placeholder works because the relevant SELECT
//! policy only checks that `app.current_user_id` is set, not that it
//! names a real user; this module's writes need one step further, since
//! `clients` schema INSERT/UPDATE/DELETE policies also require
//! `onboarding_manager`/`department_manager` -- but `begin_rls_transaction`
//! never validates `role_keys` against a real roles table, it just sets
//! them as the `app.current_user_roles` GUC verbatim (see
//! `clients::ingest`'s own live test), so passing that role list
//! directly satisfies the write policies too. There is no distinct
//! "system" role in this app's RBAC, so reusing the same client-ops
//! write gate every human write already goes through is the pragmatic
//! choice over inventing a new one for this one caller.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use parking_lot::RwLock;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::auth::begin_rls_transaction;
use crate::client_ops::audit_log;
use crate::clients::intake_mapping::{map_intake_fields, MappedCompany, MappedFacility};
use crate::clients::known_workflows::{
    CONTRACT_ORDER_WORKFLOW_ID, INTAKE_WORKFLOW_ID, MERCHANT_ACCOUNT_WORKFLOW_ID,
};
use crate::clients::person_index::{
    extract_contract_order_people, extract_intake_people, extract_merchant_account_people,
    ExtractedPerson,
};
use crate::process_street::{ProcessStreetClient, ProcessStreetError};

/// See this module's own doc comment for why a fixed, non-empty
/// placeholder is correct here, and why it also covers the write
/// policies `client_ops::vendor_format`'s read-only use of the same
/// pattern never had to.
const SYSTEM_USER_ID: Uuid = Uuid::nil();
const SYSTEM_ROLE: &str = "onboarding_manager";

/// Fallback only -- used when `client_ops.process_street_settings`
/// can't be read at all (a transient DB error), never as the normal
/// path. The settings row itself defaults to the same value.
fn default_sync_interval_hours() -> i16 {
    24
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("Process Street request failed: {0}")]
    ProcessStreet(#[from] ProcessStreetError),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncStats {
    pub workflow: &'static str,
    pub runs_seen: usize,
    pub runs_changed: usize,
    pub people_indexed: usize,
    /// Always 0 for every workflow but "intake" -- see `sync_one_run`'s
    /// own doc comment on why company/facility refresh is Intake-only.
    pub companies_refreshed: usize,
    pub facilities_refreshed: usize,
}

/// Shared, pollable progress for whichever sync run is currently in
/// flight (or last finished) -- lets `api::clients_sync`'s "Sync Now"
/// button show a live percentage instead of a bare spinner, and lets the
/// manual trigger and the nightly timer share one "is a sync already
/// running" guard (see `try_claim_running`) so a click during the
/// nightly window doesn't start a second, overlapping pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncState {
    #[default]
    Idle,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Default)]
pub struct SyncProgress {
    pub state: SyncState,
    /// Known only once every workflow's (cheap) run list has come back
    /// -- see `run_all_workflows_with_progress`. Zero while state is
    /// still `Idle`.
    pub total_runs: usize,
    /// Incremented once per run after its delta check resolves, whether
    /// that run was actually refreshed or skipped -- "processed" means
    /// "a decision was made," not "a `form-fields` fetch happened."
    pub processed_runs: usize,
    pub results: Vec<SyncStats>,
    /// Set only when `state == Failed` -- the error that stopped the
    /// run early. A failure on one workflow does not appear here; see
    /// `sync_all_workflows`'s own per-workflow error handling for that
    /// (unrelated) case, still used by the plain, progress-free path.
    pub error: Option<String>,
}

impl SyncProgress {
    pub fn percent(&self) -> u8 {
        if self.total_runs == 0 {
            return 0;
        }
        ((self.processed_runs * 100) / self.total_runs).min(100) as u8
    }
}

pub type SyncProgressHandle = Arc<RwLock<SyncProgress>>;

/// Atomically checks-and-claims the "a sync is running" slot -- `true`
/// means the caller now owns the run and must call
/// `run_all_workflows_with_progress` (which itself sets `Completed`/
/// `Failed` on every exit path); `false` means one was already in
/// progress and the caller should do nothing. The check and the claim
/// happen under the same write lock, so two simultaneous callers (the
/// nightly timer's tick and a manual click landing at the same instant)
/// can't both observe `Idle` and both proceed.
pub fn try_claim_running(progress: &SyncProgressHandle) -> bool {
    let mut guard = progress.write();
    if guard.state == SyncState::Running {
        return false;
    }
    *guard = SyncProgress {
        state: SyncState::Running,
        ..Default::default()
    };
    true
}

/// Pure decision at the heart of the delta check -- pulled out of the
/// DB/network-heavy loop below so it has its own direct unit tests, no
/// fixture or live call needed. `None` (never synced before) always
/// needs a refresh; otherwise a run only needs one when PS's own
/// `updatedDate` has moved past what was last recorded.
fn needs_refresh(previously_synced_at: Option<DateTime<Utc>>, current_updated_at: DateTime<Utc>) -> bool {
    match previously_synced_at {
        Some(prev) => prev < current_updated_at,
        None => true,
    }
}

/// What one run's own delta-refresh actually did, beyond the
/// `ps_person_index` bookkeeping `sync_one_run` always performs when a
/// refresh is needed at all -- whether an already-imported Company and/or
/// Facility record (see `company_refreshed`/`facility_refreshed`) also
/// got its fields updated from this same fresh fetch. A single Intake
/// run can match both: since `create.rs`'s 2026-09-02 change, a
/// company's source run is often also one of its own facility runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct RunSyncOutcome {
    person_index_refreshed: bool,
    people_indexed: usize,
    company_refreshed: bool,
    facility_refreshed: bool,
}

/// Computes a refreshed field value -- never overwrites a field listed
/// in `manually_edited_fields` (a human's deliberate correction), and
/// never blanks a good existing value just because this fresh pull came
/// back empty for it. Only a genuine, non-null change from Process
/// Street is ever applied. See `clients.companies`/`clients.facilities`
/// `manually_edited_fields` columns' own migration comment for why this
/// exists at all.
fn refreshed_field<T: Clone + PartialEq>(current: &Option<T>, fresh: &Option<T>, is_protected: bool) -> Option<T> {
    if is_protected {
        return current.clone();
    }
    match fresh {
        Some(_) => fresh.clone(),
        None => current.clone(),
    }
}

/// Applies a fresh Intake mapping onto a company's current fields,
/// respecting `protected_fields` (that company's own
/// `manually_edited_fields`) field by field -- mirrors
/// `clients::create::diff_company_fields`'s own field list exactly,
/// since the two are the write and read sides of the same protected set.
pub(crate) fn apply_company_refresh(current: &MappedCompany, fresh: &MappedCompany, protected_fields: &[String]) -> MappedCompany {
    let is_protected = |field: &str| protected_fields.iter().any(|p| p == field);
    MappedCompany {
        legal_name: refreshed_field(&current.legal_name, &fresh.legal_name, is_protected("legal_name")),
        corporate_email: refreshed_field(
            &current.corporate_email,
            &fresh.corporate_email,
            is_protected("corporate_email"),
        ),
        corporate_phone: refreshed_field(
            &current.corporate_phone,
            &fresh.corporate_phone,
            is_protected("corporate_phone"),
        ),
        corporate_address_street: refreshed_field(
            &current.corporate_address_street,
            &fresh.corporate_address_street,
            is_protected("corporate_address_street"),
        ),
        corporate_address_city: refreshed_field(
            &current.corporate_address_city,
            &fresh.corporate_address_city,
            is_protected("corporate_address_city"),
        ),
        corporate_address_state: refreshed_field(
            &current.corporate_address_state,
            &fresh.corporate_address_state,
            is_protected("corporate_address_state"),
        ),
        corporate_address_zip: refreshed_field(
            &current.corporate_address_zip,
            &fresh.corporate_address_zip,
            is_protected("corporate_address_zip"),
        ),
        subdomain: refreshed_field(&current.subdomain, &fresh.subdomain, is_protected("subdomain")),
        accepted_payment_methods: refreshed_field(
            &current.accepted_payment_methods,
            &fresh.accepted_payment_methods,
            is_protected("accepted_payment_methods"),
        ),
        accounting_basis: refreshed_field(
            &current.accounting_basis,
            &fresh.accounting_basis,
            is_protected("accounting_basis"),
        ),
        payment_scheme: refreshed_field(
            &current.payment_scheme,
            &fresh.payment_scheme,
            is_protected("payment_scheme"),
        ),
        offers_tenant_insurance_raw: refreshed_field(
            &current.offers_tenant_insurance_raw,
            &fresh.offers_tenant_insurance_raw,
            is_protected("offers_tenant_insurance_raw"),
        ),
        insurance_provider: refreshed_field(
            &current.insurance_provider,
            &fresh.insurance_provider,
            is_protected("insurance_provider"),
        ),
        // `map_intake_fields` never sets `MappedCompany::website_url`
        // (no PS field of its own -- see that field's own doc comment),
        // so `fresh.website_url` is always `None` here and
        // `refreshed_field`'s own "a fresh null never blanks a good
        // value" rule means this always just preserves whatever the
        // confirmation-screen fallback already copied in. Included
        // anyway for the same reason `is_protected` still gates it: the
        // day this ever gains a real PS source, this line already does
        // the right thing.
        website_url: refreshed_field(&current.website_url, &fresh.website_url, is_protected("website_url")),
    }
}

/// `apply_company_refresh`'s counterpart for `MappedFacility` --
/// `go_live_date` is always carried through from `current` untouched,
/// same "never touched by anything but PS's own original mapping" rule
/// `clients::create::apply_facility_overrides` already established.
pub(crate) fn apply_facility_refresh(current: &MappedFacility, fresh: &MappedFacility, protected_fields: &[String]) -> MappedFacility {
    let is_protected = |field: &str| protected_fields.iter().any(|p| p == field);
    MappedFacility {
        name: refreshed_field(&current.name, &fresh.name, is_protected("name")),
        street_address: refreshed_field(&current.street_address, &fresh.street_address, is_protected("street_address")),
        city: refreshed_field(&current.city, &fresh.city, is_protected("city")),
        state: refreshed_field(&current.state, &fresh.state, is_protected("state")),
        zip: refreshed_field(&current.zip, &fresh.zip, is_protected("zip")),
        phone: refreshed_field(&current.phone, &fresh.phone, is_protected("phone")),
        email: refreshed_field(&current.email, &fresh.email, is_protected("email")),
        units_count: refreshed_field(&current.units_count, &fresh.units_count, is_protected("units_count")),
        primary_storage_offering: refreshed_field(
            &current.primary_storage_offering,
            &fresh.primary_storage_offering,
            is_protected("primary_storage_offering"),
        ),
        previous_pms: refreshed_field(&current.previous_pms, &fresh.previous_pms, is_protected("previous_pms")),
        access_control_system: refreshed_field(
            &current.access_control_system,
            &fresh.access_control_system,
            is_protected("access_control_system"),
        ),
        dropbox_folder_url: refreshed_field(
            &current.dropbox_folder_url,
            &fresh.dropbox_folder_url,
            is_protected("dropbox_folder_url"),
        ),
        subdomain: refreshed_field(&current.subdomain, &fresh.subdomain, is_protected("subdomain")),
        subdomain_exists_in_qms_raw: refreshed_field(
            &current.subdomain_exists_in_qms_raw,
            &fresh.subdomain_exists_in_qms_raw,
            is_protected("subdomain_exists_in_qms_raw"),
        ),
        system_email: refreshed_field(&current.system_email, &fresh.system_email, is_protected("system_email")),
        website_url: refreshed_field(&current.website_url, &fresh.website_url, is_protected("website_url")),
        go_live_date: current.go_live_date,
    }
}

/// Field names where two `MappedFacility` values differ -- `go_live_date`
/// is deliberately excluded, same reasoning as everywhere else in this
/// module: nothing but the original PS mapping ever sets it. Used by
/// `api::clients_resync` to tell "this field would change on refresh"
/// (`current` vs. the fresh pull) apart from "this field would change
/// AND it's protected" (an actual conflict needing the caller's choice)
/// -- `clients::create::diff_company_fields` is `MappedCompany`'s own
/// counterpart, directly reusable there since both its arguments are
/// already `MappedCompany`.
pub(crate) fn facility_fields_that_differ(a: &MappedFacility, b: &MappedFacility) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if a.name != b.name {
        changed.push("name");
    }
    if a.street_address != b.street_address {
        changed.push("street_address");
    }
    if a.city != b.city {
        changed.push("city");
    }
    if a.state != b.state {
        changed.push("state");
    }
    if a.zip != b.zip {
        changed.push("zip");
    }
    if a.phone != b.phone {
        changed.push("phone");
    }
    if a.email != b.email {
        changed.push("email");
    }
    if a.units_count != b.units_count {
        changed.push("units_count");
    }
    if a.primary_storage_offering != b.primary_storage_offering {
        changed.push("primary_storage_offering");
    }
    if a.previous_pms != b.previous_pms {
        changed.push("previous_pms");
    }
    if a.access_control_system != b.access_control_system {
        changed.push("access_control_system");
    }
    if a.dropbox_folder_url != b.dropbox_folder_url {
        changed.push("dropbox_folder_url");
    }
    if a.subdomain != b.subdomain {
        changed.push("subdomain");
    }
    if a.subdomain_exists_in_qms_raw != b.subdomain_exists_in_qms_raw {
        changed.push("subdomain_exists_in_qms_raw");
    }
    if a.system_email != b.system_email {
        changed.push("system_email");
    }
    if a.website_url != b.website_url {
        changed.push("website_url");
    }
    changed
}

/// Reads one field's current string value off a `MappedCompany` by name
/// -- used by `api::clients_resync` to describe a conflict generically
/// (field name + both candidate values) without a giant match at the
/// call site. `None` for a name this type doesn't have, which never
/// happens in practice since callers only ever pass names this same
/// module's own diff functions produced.
pub(crate) fn company_field_value(company: &MappedCompany, field: &str) -> Option<String> {
    match field {
        "legal_name" => company.legal_name.clone(),
        "corporate_email" => company.corporate_email.clone(),
        "corporate_phone" => company.corporate_phone.clone(),
        "corporate_address_street" => company.corporate_address_street.clone(),
        "corporate_address_city" => company.corporate_address_city.clone(),
        "corporate_address_state" => company.corporate_address_state.clone(),
        "corporate_address_zip" => company.corporate_address_zip.clone(),
        "subdomain" => company.subdomain.clone(),
        "accepted_payment_methods" => company.accepted_payment_methods.clone(),
        "accounting_basis" => company.accounting_basis.clone(),
        "payment_scheme" => company.payment_scheme.clone(),
        "offers_tenant_insurance_raw" => company.offers_tenant_insurance_raw.clone(),
        "insurance_provider" => company.insurance_provider.clone(),
        "website_url" => company.website_url.clone(),
        _ => None,
    }
}

/// `company_field_value`'s counterpart for `MappedFacility`.
pub(crate) fn facility_field_value(facility: &MappedFacility, field: &str) -> Option<String> {
    match field {
        "name" => facility.name.clone(),
        "street_address" => facility.street_address.clone(),
        "city" => facility.city.clone(),
        "state" => facility.state.clone(),
        "zip" => facility.zip.clone(),
        "phone" => facility.phone.clone(),
        "email" => facility.email.clone(),
        "units_count" => facility.units_count.map(|n| n.to_string()),
        "primary_storage_offering" => facility.primary_storage_offering.clone(),
        "previous_pms" => facility.previous_pms.clone(),
        "access_control_system" => facility.access_control_system.clone(),
        "dropbox_folder_url" => facility.dropbox_folder_url.clone(),
        "subdomain" => facility.subdomain.clone(),
        "subdomain_exists_in_qms_raw" => facility.subdomain_exists_in_qms_raw.clone(),
        "system_email" => facility.system_email.clone(),
        "website_url" => facility.website_url.clone(),
        _ => None,
    }
}

/// Refreshes the one `clients.companies` row (if any) whose
/// `ps_intake_run_id` matches this Intake run, applying `fresh` field by
/// field through `apply_company_refresh`. A no-op (returns `false`,
/// touches nothing) when no company matches this run at all, or when
/// every field that would change is protected/already up to date.
/// A `clients.companies` row's current refreshable fields --
/// `#[derive(FromRow)]` rather than a tuple for the same reason
/// `ExistingFacilityRow` below is: sqlx's tuple `FromRow` impls only go
/// up to a handful of elements, and the 2026-09-03 Financial Information
/// fields pushed this table past that.
#[derive(sqlx::FromRow)]
struct ExistingCompanyRow {
    id: Uuid,
    legal_name: String,
    corporate_email: Option<String>,
    corporate_phone: Option<String>,
    corporate_address_street: Option<String>,
    corporate_address_city: Option<String>,
    corporate_address_state: Option<String>,
    corporate_address_zip: Option<String>,
    subdomain: Option<String>,
    accepted_payment_methods: Option<String>,
    accounting_basis: Option<String>,
    payment_scheme: Option<String>,
    offers_tenant_insurance_raw: Option<String>,
    insurance_provider: Option<String>,
    website_url: Option<String>,
    manually_edited_fields: Vec<String>,
}

async fn refresh_matching_company(
    tx: &mut Transaction<'_, Postgres>,
    run_id: &str,
    fresh: &MappedCompany,
) -> Result<bool, SyncError> {
    let existing: Option<ExistingCompanyRow> = sqlx::query_as(
        "SELECT id, legal_name, corporate_email, corporate_phone, corporate_address_street, \
         corporate_address_city, corporate_address_state, corporate_address_zip, subdomain, \
         accepted_payment_methods, accounting_basis, payment_scheme, offers_tenant_insurance_raw, \
         insurance_provider, website_url, manually_edited_fields \
         FROM clients.companies WHERE ps_intake_run_id = $1",
    )
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await?;

    let Some(ExistingCompanyRow {
        id,
        legal_name,
        corporate_email,
        corporate_phone,
        corporate_address_street,
        corporate_address_city,
        corporate_address_state,
        corporate_address_zip,
        subdomain,
        accepted_payment_methods,
        accounting_basis,
        payment_scheme,
        offers_tenant_insurance_raw,
        insurance_provider,
        website_url,
        manually_edited_fields,
    }) = existing
    else {
        return Ok(false);
    };

    let current = MappedCompany {
        legal_name: Some(legal_name),
        corporate_email,
        corporate_phone,
        corporate_address_street,
        corporate_address_city,
        corporate_address_state,
        corporate_address_zip,
        subdomain,
        accepted_payment_methods,
        accounting_basis,
        payment_scheme,
        offers_tenant_insurance_raw,
        insurance_provider,
        website_url,
    };

    let refreshed = apply_company_refresh(&current, fresh, &manually_edited_fields);
    if refreshed == current {
        return Ok(false);
    }

    let refreshed_legal_name = refreshed
        .legal_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("(unnamed company)");

    sqlx::query(
        "UPDATE clients.companies SET legal_name = $1, corporate_email = $2, corporate_phone = $3, \
         corporate_address_street = $4, corporate_address_city = $5, corporate_address_state = $6, \
         corporate_address_zip = $7, subdomain = $8, accepted_payment_methods = $9, \
         accounting_basis = $10, payment_scheme = $11, offers_tenant_insurance_raw = $12, \
         insurance_provider = $13, website_url = $14, last_synced_at = now() WHERE id = $15",
    )
    .bind(refreshed_legal_name)
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
    .bind(id)
    .execute(&mut **tx)
    .await?;

    Ok(true)
}

/// A `clients.facilities` row's current refreshable fields --
/// `#[derive(FromRow)]` rather than a giant tuple purely because sqlx's
/// tuple `FromRow` impls only go up to a handful of elements, well short
/// of this table's real column count.
#[derive(sqlx::FromRow)]
struct ExistingFacilityRow {
    id: Uuid,
    name: String,
    street_address: Option<String>,
    city: Option<String>,
    state: Option<String>,
    zip: Option<String>,
    phone: Option<String>,
    email: Option<String>,
    units_count: Option<i32>,
    primary_storage_offering: Option<String>,
    previous_pms: Option<String>,
    access_control_system: Option<String>,
    go_live_date: Option<chrono::NaiveDate>,
    dropbox_folder_url: Option<String>,
    subdomain: Option<String>,
    subdomain_exists_in_qms_raw: Option<String>,
    system_email: Option<String>,
    website_url: Option<String>,
    manually_edited_fields: Vec<String>,
}

/// `refresh_matching_company`'s counterpart for `clients.facilities`.
async fn refresh_matching_facility(
    tx: &mut Transaction<'_, Postgres>,
    run_id: &str,
    fresh: &MappedFacility,
) -> Result<bool, SyncError> {
    let existing: Option<ExistingFacilityRow> = sqlx::query_as(
        "SELECT id, name, street_address, city, state, zip, phone, email, units_count, \
         primary_storage_offering, previous_pms, access_control_system, go_live_date, \
         dropbox_folder_url, subdomain, subdomain_exists_in_qms_raw, system_email, website_url, \
         manually_edited_fields \
         FROM clients.facilities WHERE ps_intake_run_id = $1",
    )
    .bind(run_id)
    .fetch_optional(&mut **tx)
    .await?;

    let Some(ExistingFacilityRow {
        id,
        name,
        street_address,
        city,
        state,
        zip,
        phone,
        email,
        units_count,
        primary_storage_offering,
        previous_pms,
        access_control_system,
        go_live_date,
        dropbox_folder_url,
        subdomain,
        subdomain_exists_in_qms_raw,
        system_email,
        website_url,
        manually_edited_fields,
    }) = existing
    else {
        return Ok(false);
    };

    let current = MappedFacility {
        name: Some(name),
        street_address,
        city,
        state,
        zip,
        phone,
        email,
        units_count,
        primary_storage_offering,
        previous_pms,
        access_control_system,
        go_live_date,
        dropbox_folder_url,
        subdomain,
        subdomain_exists_in_qms_raw,
        system_email,
        website_url,
    };

    let refreshed = apply_facility_refresh(&current, fresh, &manually_edited_fields);
    if refreshed == current {
        return Ok(false);
    }

    sqlx::query(
        "UPDATE clients.facilities SET name = $1, street_address = $2, city = $3, state = $4, \
         zip = $5, phone = $6, email = $7, units_count = $8, primary_storage_offering = $9, \
         previous_pms = $10, access_control_system = $11, dropbox_folder_url = $12, \
         subdomain = $13, subdomain_exists_in_qms_raw = $14, system_email = $15, website_url = $16, \
         last_synced_at = now() WHERE id = $17",
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
    .bind(id)
    .execute(&mut **tx)
    .await?;

    Ok(true)
}

type ExtractFn = fn(&[crate::process_street::FormField]) -> Vec<ExtractedPerson>;

const WORKFLOWS: &[(&str, &str, ExtractFn)] = &[
    (INTAKE_WORKFLOW_ID, "intake", extract_intake_people),
    (MERCHANT_ACCOUNT_WORKFLOW_ID, "merchant_account", extract_merchant_account_people),
    (CONTRACT_ORDER_WORKFLOW_ID, "contract_order", extract_contract_order_people),
];

/// Applies the delta check to exactly one run, refreshing it (deleting
/// and re-inserting its `ps_person_index` rows, upserting
/// `ps_sync_state`, and -- for an Intake run only -- refreshing any
/// already-imported Company/Facility whose own `ps_intake_run_id`
/// matches, see `refresh_matching_company`/`refresh_matching_facility`)
/// only when `needs_refresh` says so. Split out of `sync_workflow_within`
/// so `live_tests` can prove the skip behavior against one specific
/// known run without paying for a real `/form-fields` fetch on every
/// other real run in the workflow -- the expensive call this whole
/// module exists to avoid making unnecessarily.
///
/// Company/Facility refresh is Intake-only for now: a company's fields
/// are seeded from whichever facility's own Intake run answered "first
/// time = Yes" (see `clients.companies.ps_intake_run_id`'s own migration
/// comment), not from a persisted link to a Merchant Account run -- there
/// is no such link stored today, so a later change to that Merchant
/// Account run's own data has nothing to refresh against yet.
async fn sync_one_run(
    tx: &mut Transaction<'_, Postgres>,
    client: &ProcessStreetClient,
    workflow_key: &'static str,
    run: &crate::process_street::WorkflowRun,
    previously_synced_at: Option<DateTime<Utc>>,
    extract: ExtractFn,
) -> Result<RunSyncOutcome, SyncError> {
    if !needs_refresh(previously_synced_at, run.updated_at()) {
        return Ok(RunSyncOutcome::default());
    }

    let fields = client.get_run_form_fields(&run.id).await?;
    let people = extract(&fields);

    sqlx::query("DELETE FROM clients.ps_person_index WHERE workflow = $1 AND ps_run_id = $2")
        .bind(workflow_key)
        .bind(&run.id)
        .execute(&mut **tx)
        .await?;

    for person in &people {
        sqlx::query(
            "INSERT INTO clients.ps_person_index
                 (workflow, ps_run_id, run_name, full_name, email, phone, role)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(workflow_key)
        .bind(&run.id)
        .bind(&run.name)
        .bind(&person.full_name)
        .bind(&person.email)
        .bind(&person.phone)
        .bind(person.role)
        .execute(&mut **tx)
        .await?;
    }

    sqlx::query(
        "INSERT INTO clients.ps_sync_state (workflow, ps_run_id, run_name, ps_updated_at, last_synced_at)
         VALUES ($1, $2, $3, $4, now())
         ON CONFLICT (workflow, ps_run_id) DO UPDATE SET
             run_name = EXCLUDED.run_name,
             ps_updated_at = EXCLUDED.ps_updated_at,
             last_synced_at = now()",
    )
    .bind(workflow_key)
    .bind(&run.id)
    .bind(&run.name)
    .bind(run.updated_at())
    .execute(&mut **tx)
    .await?;

    let (company_refreshed, facility_refreshed) = if workflow_key == "intake" {
        let mapped = map_intake_fields(&fields);
        let company_refreshed = refresh_matching_company(tx, &run.id, &mapped.company).await?;
        let facility_refreshed = refresh_matching_facility(tx, &run.id, &mapped.facility).await?;
        (company_refreshed, facility_refreshed)
    } else {
        (false, false)
    };

    Ok(RunSyncOutcome {
        person_index_refreshed: true,
        people_indexed: people.len(),
        company_refreshed,
        facility_refreshed,
    })
}

/// Syncs a pre-fetched list of one workflow's runs into
/// `ps_sync_state`/`ps_person_index` within an already-open transaction
/// -- the caller decides whether to commit or roll back, same
/// discipline `clients::ingest::ingest_facility` and every
/// `clients::repository` function already use. Takes `runs` rather than
/// a `workflow_id` and listing them itself so `run_all_workflows_with_progress`
/// can list every workflow up front (to know the real total before
/// processing any of them) without a second, redundant list call here.
///
/// `on_processed` fires once per run after its delta check resolves --
/// `run_all_workflows_with_progress` uses it to advance a shared
/// progress counter; the plain `sync_workflow` entry point below passes
/// a no-op.
async fn sync_runs_within(
    tx: &mut Transaction<'_, Postgres>,
    client: &ProcessStreetClient,
    workflow_key: &'static str,
    runs: &[crate::process_street::WorkflowRun],
    extract: ExtractFn,
    mut on_processed: impl FnMut(),
) -> Result<SyncStats, SyncError> {
    let existing: HashMap<String, DateTime<Utc>> = sqlx::query_as(
        "SELECT ps_run_id, ps_updated_at FROM clients.ps_sync_state WHERE workflow = $1",
    )
    .bind(workflow_key)
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .collect();

    let mut runs_changed = 0;
    let mut people_indexed = 0;
    let mut companies_refreshed = 0;
    let mut facilities_refreshed = 0;

    for run in runs {
        let outcome = sync_one_run(
            tx,
            client,
            workflow_key,
            run,
            existing.get(&run.id).copied(),
            extract,
        )
        .await?;
        if outcome.person_index_refreshed {
            runs_changed += 1;
            people_indexed += outcome.people_indexed;
        }
        if outcome.company_refreshed {
            companies_refreshed += 1;
        }
        if outcome.facility_refreshed {
            facilities_refreshed += 1;
        }
        on_processed();
    }

    Ok(SyncStats {
        workflow: workflow_key,
        runs_seen: runs.len(),
        runs_changed,
        people_indexed,
        companies_refreshed,
        facilities_refreshed,
    })
}

/// The entry point both the nightly timer and the manual "Sync Now"
/// endpoint use -- lists every workflow's runs up front so `progress.total_runs`
/// is known (and
/// therefore a meaningful percentage is showable) before any per-run
/// work starts, and stops at the first error rather than continuing
/// past it (a partial percentage that then silently stalls is worse
/// here than a clearly-`Failed` state the UI can show).
///
/// Callers must hold the claim from `try_claim_running` before calling
/// this -- it sets `Completed`/`Failed` on every exit path but does not
/// itself guard against two concurrent invocations.
///
/// `actor_user_id` is who to credit in the Activity Log for this run --
/// `SYSTEM_USER_ID` for the nightly timer, the real caller's id for the
/// manual "Sync Now"/scoped Re-sync triggers -- recorded on both
/// `SYNC_COMPLETED` and `SYNC_FAILED` (see `client_ops::audit_log`'s own
/// module doc on why a failed Process Street call belongs in the same
/// trail as every other activity, not just server logs).
pub async fn run_all_workflows_with_progress(
    client: &ProcessStreetClient,
    db: &PgPool,
    progress: &SyncProgressHandle,
    actor_user_id: Uuid,
) {
    let mut per_workflow_runs = Vec::with_capacity(WORKFLOWS.len());
    for (workflow_id, workflow_key, extract) in WORKFLOWS {
        match client.list_workflow_runs(workflow_id).await {
            Ok(runs) => per_workflow_runs.push((*workflow_key, *extract, runs)),
            Err(err) => {
                fail(db, progress, actor_user_id, err.to_string()).await;
                return;
            }
        }
    }

    let total_runs: usize = per_workflow_runs.iter().map(|(_, _, runs)| runs.len()).sum();
    progress.write().total_runs = total_runs;

    let mut results = Vec::with_capacity(per_workflow_runs.len());

    for (workflow_key, extract, runs) in &per_workflow_runs {
        let mut tx = match begin_rls_transaction(db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()]).await {
            Ok(tx) => tx,
            Err(err) => {
                fail(db, progress, actor_user_id, err.to_string()).await;
                return;
            }
        };

        let stats_result = sync_runs_within(&mut tx, client, workflow_key, runs, *extract, || {
            progress.write().processed_runs += 1;
        })
        .await;

        let stats = match stats_result {
            Ok(stats) => stats,
            Err(err) => {
                let _ = tx.rollback().await;
                fail(db, progress, actor_user_id, err.to_string()).await;
                return;
            }
        };

        if let Err(err) = tx.commit().await {
            fail(db, progress, actor_user_id, err.to_string()).await;
            return;
        }

        results.push(stats);
    }

    let companies_refreshed: usize = results.iter().map(|s| s.companies_refreshed).sum();
    let facilities_refreshed: usize = results.iter().map(|s| s.facilities_refreshed).sum();

    audit_log::record(
        db,
        audit_log::event::SYNC_COMPLETED,
        actor_user_id,
        "sync_run",
        None,
        audit_log::Change::none(),
        None,
        None,
        serde_json::json!({
            "total_runs": total_runs,
            "companies_refreshed": companies_refreshed,
            "facilities_refreshed": facilities_refreshed,
            "results": results.iter().map(|s| serde_json::json!({
                "workflow": s.workflow,
                "runs_seen": s.runs_seen,
                "runs_changed": s.runs_changed,
            })).collect::<Vec<_>>(),
        }),
    )
    .await;

    let mut guard = progress.write();
    guard.state = SyncState::Completed;
    guard.results = results;
}

/// Marks the shared progress handle `Failed` and records `SYNC_FAILED`
/// in the Activity Log -- e.g. Process Street was unreachable or
/// returned an error partway through. See this module's own doc comment
/// on why a sync failure is worth its own audited event, not just a
/// server-log line.
async fn fail(db: &PgPool, progress: &SyncProgressHandle, actor_user_id: Uuid, message: String) {
    audit_log::record(
        db,
        audit_log::event::SYNC_FAILED,
        actor_user_id,
        "sync_run",
        None,
        audit_log::Change::none(),
        None,
        None,
        serde_json::json!({ "error": message }),
    )
    .await;

    let mut guard = progress.write();
    guard.state = SyncState::Failed;
    guard.error = Some(message);
}

/// Reads `client_ops.process_street_settings.sync_interval_hours` on the
/// same system role/RLS pattern as everything else in this module. Falls
/// back to `default_sync_interval_hours()` (never a panic, never
/// blocking the loop forever) on any read failure -- a transient DB
/// hiccup should delay this cycle's sync, not crash the background task.
async fn fetch_sync_interval_hours(db: &PgPool) -> i16 {
    let result: Result<(i16,), sqlx::Error> = async {
        let mut tx = begin_rls_transaction(db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()]).await?;
        let row = sqlx::query_as(
            "SELECT sync_interval_hours FROM client_ops.process_street_settings WHERE id = 1",
        )
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(row)
    }
    .await;

    match result {
        Ok((interval_hours,)) => interval_hours,
        Err(err) => {
            tracing::error!(
                error = %err,
                "failed to read the configured Process Street sync interval; defaulting to 24h for this cycle"
            );
            default_sync_interval_hours()
        }
    }
}

/// Sleeps for the currently configured interval before the next sync
/// tick -- re-read on every call, not cached, so a settings change
/// (`api::process_street_settings`) takes effect on the very next cycle
/// without needing a server restart. Unlike the old fixed-time-of-day
/// schedule this replaces, there is no "next occurrence" to compute:
/// every tick is simply "interval hours after the last one finished",
/// which is exactly what sleeping this long, then looping, already does
/// -- see `clients::sync`'s own module doc for why a much shorter
/// interval than the old once-daily default is now realistic at all
/// (the delta mechanism makes an unchanged run essentially free).
async fn sleep_until_next_scheduled_sync(db: &PgPool) {
    let interval_hours = fetch_sync_interval_hours(db).await;
    let sleep_duration = std::time::Duration::from_secs((interval_hours.max(1) as u64) * 3600);

    tracing::info!(
        next_sync_at = %(Utc::now() + ChronoDuration::hours(i64::from(interval_hours))),
        interval_hours,
        "Process Street sync scheduled"
    );

    tokio::time::sleep(sleep_duration).await;
}

/// Spawns the scheduled sync loop -- runs every configured
/// `sync_interval_hours` (`client_ops.process_street_settings`, default
/// 24) or when `api::clients_sync::start_sync` triggers one manually.
/// Deliberately does NOT also fire immediately on startup the
/// way `client_ops::vendor_format::start_refresh_task` does -- that
/// task's cache would otherwise sit empty until the first tick and
/// block real request handling; an empty `ps_person_index` just means
/// fewer search results, and firing on every restart would mean every
/// local dev run or every prod deploy kicks off a real, resource-
/// competing sync against the live PS API whether anyone wants one
/// right then or not (confirmed directly, 2026-08-31: a restart-
/// triggered first sync measurably slowed down an unrelated live test
/// running against the same dev database at the same time).
///
/// Shares `progress` with the manual "Sync Now" endpoint
/// (`api::clients_sync`) -- `try_claim_running` means a scheduled run
/// that lands while a manual sync is still in flight (or vice versa)
/// just skips rather than starting a second, overlapping pass.
pub fn start_background_sync_task(
    client: Arc<ProcessStreetClient>,
    db: PgPool,
    progress: SyncProgressHandle,
) {
    tokio::spawn(async move {
        loop {
            sleep_until_next_scheduled_sync(&db).await;

            if !try_claim_running(&progress) {
                tracing::warn!(
                    "Skipping this Process Street sync tick -- a sync (manual or scheduled) is already running"
                );
                continue;
            }

            run_all_workflows_with_progress(&client, &db, &progress, SYSTEM_USER_ID).await;

            let finished = progress.read().clone();
            match finished.state {
                SyncState::Completed => tracing::info!(
                    runs_seen = finished.total_runs,
                    results = ?finished.results,
                    "Process Street person-index sync completed"
                ),
                SyncState::Failed => tracing::error!(
                    error = finished.error.as_deref().unwrap_or("unknown error"),
                    "Process Street person-index sync failed"
                ),
                SyncState::Idle | SyncState::Running => {
                    // Unreachable in practice -- run_all_workflows_with_progress
                    // always leaves Completed or Failed on every exit path.
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_synced_before_always_needs_refresh() {
        assert!(needs_refresh(None, Utc::now()));
    }

    #[test]
    fn unchanged_updated_at_does_not_need_refresh() {
        let t = Utc::now();
        assert!(!needs_refresh(Some(t), t));
    }

    #[test]
    fn a_later_updated_at_needs_refresh() {
        let earlier = Utc::now() - ChronoDuration::days(1);
        let later = Utc::now();
        assert!(needs_refresh(Some(earlier), later));
    }

    #[test]
    fn an_updated_at_that_moved_backward_does_not_need_refresh() {
        // Should never happen against the real API, but the comparison
        // itself must not treat "earlier than what's recorded" as a
        // reason to refresh -- only strictly-later does.
        let later = Utc::now();
        let earlier = later - ChronoDuration::days(1);
        assert!(!needs_refresh(Some(later), earlier));
    }

    fn company(legal_name: &str) -> MappedCompany {
        MappedCompany {
            legal_name: Some(legal_name.to_string()),
            corporate_email: Some("office@example.com".to_string()),
            corporate_phone: Some("555-000-0000".to_string()),
            corporate_address_street: Some("1 Example St".to_string()),
            corporate_address_city: Some("Example City".to_string()),
            corporate_address_state: Some("IL".to_string()),
            corporate_address_zip: Some("60000".to_string()),
            subdomain: Some("example.qms-email.com".to_string()),
            accepted_payment_methods: Some("Credit Card, ACH".to_string()),
            accounting_basis: Some("Cash".to_string()),
            payment_scheme: Some("Advance".to_string()),
            offers_tenant_insurance_raw: Some("Yes".to_string()),
            insurance_provider: Some("Example Insurance Co".to_string()),
            website_url: Some("https://example.com".to_string()),
        }
    }

    #[test]
    fn refreshed_field_takes_a_new_non_null_value_when_not_protected() {
        let current = Some("old".to_string());
        let fresh = Some("new".to_string());

        assert_eq!(refreshed_field(&current, &fresh, false), Some("new".to_string()));
    }

    #[test]
    fn refreshed_field_never_overwrites_a_protected_field() {
        let current = Some("manually corrected".to_string());
        let fresh = Some("stale ps value".to_string());

        assert_eq!(
            refreshed_field(&current, &fresh, true),
            Some("manually corrected".to_string())
        );
    }

    #[test]
    fn refreshed_field_never_blanks_a_good_value_with_a_fresh_null() {
        // PS returning nothing for a field it previously had a value for
        // must not be read as "clear it" -- only a genuine new value
        // ever overwrites, protected or not.
        let current = Some("existing value".to_string());
        let fresh: Option<String> = None;

        assert_eq!(refreshed_field(&current, &fresh, false), current);
    }

    #[test]
    fn apply_company_refresh_updates_unprotected_fields_that_changed() {
        let current = company("Old Legal Name LLC");
        let fresh = company("Prairie Enterprises LLC");

        let refreshed = apply_company_refresh(&current, &fresh, &[]);

        assert_eq!(refreshed.legal_name.as_deref(), Some("Prairie Enterprises LLC"));
    }

    #[test]
    fn apply_company_refresh_leaves_a_manually_edited_field_untouched() {
        let current = company("Manually Corrected LLC");
        let fresh = company("Stale PS Legal Name LLC");
        let protected = vec!["legal_name".to_string()];

        let refreshed = apply_company_refresh(&current, &fresh, &protected);

        assert_eq!(refreshed.legal_name.as_deref(), Some("Manually Corrected LLC"));
        // Every other field is still free to refresh normally.
        assert_eq!(refreshed.corporate_email, fresh.corporate_email);
    }

    #[test]
    fn apply_facility_refresh_never_touches_go_live_date() {
        let current = MappedFacility {
            go_live_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
            ..fully_mapped_facility()
        };
        let fresh = MappedFacility {
            go_live_date: chrono::NaiveDate::from_ymd_opt(2026, 6, 1),
            ..fully_mapped_facility()
        };

        let refreshed = apply_facility_refresh(&current, &fresh, &[]);

        assert_eq!(refreshed.go_live_date, current.go_live_date);
    }

    #[test]
    fn apply_facility_refresh_leaves_a_manually_edited_field_untouched() {
        let current = MappedFacility {
            phone: Some("555-CORRECTED".to_string()),
            ..fully_mapped_facility()
        };
        let fresh = MappedFacility {
            phone: Some("555-STALE".to_string()),
            name: Some("Updated Facility Name".to_string()),
            ..fully_mapped_facility()
        };
        let protected = vec!["phone".to_string()];

        let refreshed = apply_facility_refresh(&current, &fresh, &protected);

        assert_eq!(refreshed.phone.as_deref(), Some("555-CORRECTED"));
        assert_eq!(refreshed.name.as_deref(), Some("Updated Facility Name"));
    }

    fn fully_mapped_facility() -> MappedFacility {
        MappedFacility {
            name: Some("Example Facility".to_string()),
            street_address: Some("1 Example St".to_string()),
            city: Some("Example City".to_string()),
            state: Some("IL".to_string()),
            zip: Some("60000".to_string()),
            phone: Some("555-000-0000".to_string()),
            email: Some("facility@example.com".to_string()),
            units_count: Some(100),
            primary_storage_offering: Some("Standard Self-Storage".to_string()),
            previous_pms: Some("3rd Party PMS".to_string()),
            access_control_system: Some("Keypad".to_string()),
            go_live_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1),
            dropbox_folder_url: Some("https://example.com/dropbox".to_string()),
            subdomain: Some("example".to_string()),
            subdomain_exists_in_qms_raw: Some("No".to_string()),
            system_email: Some("system@example.com".to_string()),
            website_url: Some("https://example.com".to_string()),
        }
    }

}

#[cfg(test)]
mod live_tests {
    use serial_test::serial;

    use crate::process_street::{ProcessStreetClient, ProcessStreetConfig};

    use super::*;

    /// Proves the delta-sync pipeline end to end against the real PS
    /// API and a real, migrated Postgres, scoped to exactly one known
    /// run (Highway 20's Intake run) rather than a whole workflow --
    /// looked up via the cheap `search_workflow_runs_by_name` list call,
    /// so this test's only expensive `/form-fields` fetch is the single
    /// one the first sync pass legitimately needs, not one per every
    /// real Intake run in the org.
    ///
    /// First pass: never-synced-before, so it must refresh and index
    /// real people. Second pass, against the very same run object (same
    /// `updated_at`, unless someone edits it in PS in the few
    /// milliseconds between the two calls in this test): must skip
    /// entirely -- the actual delta behavior this module exists for.
    /// Both run inside one uncommitted transaction, rolled back at the
    /// end so nothing persists.
    ///
    /// `#[ignore]`d for the same reason every other live test in this
    /// crate is: needs a real, reachable Postgres AND a real
    /// `PROCESS_STREET_API_KEY`. Run explicitly with
    /// `cargo test -- --ignored sync_one_run_indexes_a_real_run_and_skips_an_unchanged_one`.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres AND a real Process Street API key -- see doc comment"]
    #[serial(client_pii_encryption_key_env)]
    async fn sync_one_run_indexes_a_real_run_and_skips_an_unchanged_one() {
        let _ = dotenvy::from_filename(".env.local");

        let ps_config =
            ProcessStreetConfig::from_env().expect("PROCESS_STREET_API_KEY must be set in .env.local");
        let client = ProcessStreetClient::new(ps_config);

        let matches = client
            .search_workflow_runs_by_name(INTAKE_WORKFLOW_ID, "highway")
            .await
            .expect("search must succeed against the live API");
        let run = matches
            .into_iter()
            .find(|r| r.name == "Highway 20 Self Storage - QMS Onboarding")
            .expect("Highway 20's Intake run must be found");

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let mut tx = begin_rls_transaction(&db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()])
            .await
            .expect("beginning an RLS transaction must succeed");

        let first_outcome = sync_one_run(&mut tx, &client, "intake", &run, None, extract_intake_people)
            .await
            .expect("first sync pass must succeed against the live API");

        assert!(
            first_outcome.person_index_refreshed,
            "a never-synced-before run must always refresh"
        );
        assert!(
            first_outcome.people_indexed > 0,
            "at least one real Owner/DM/Manager person must have been indexed"
        );

        let (indexed_count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM clients.ps_person_index WHERE workflow = 'intake' AND ps_run_id = $1",
        )
        .bind(&run.id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert!(indexed_count > 0);

        // Second pass against the same run, now passing its own
        // just-recorded `updated_at` as `previously_synced_at` -- must
        // be skipped, the actual delta behavior this module exists for.
        let second_outcome = sync_one_run(
            &mut tx,
            &client,
            "intake",
            &run,
            Some(run.updated_at()),
            extract_intake_people,
        )
        .await
        .expect("second sync pass must succeed");

        assert!(
            !second_outcome.person_index_refreshed,
            "an unchanged run must not need re-fetching"
        );
        assert_eq!(second_outcome.people_indexed, 0);

        tx.rollback()
            .await
            .expect("rollback must succeed -- this is a one-time check, not a real sync");
    }

    /// Proves `refresh_matching_facility`'s hand-written UPDATE is
    /// actually valid against the real, migrated schema -- nothing about
    /// this module's plain dynamic SQL is checked at compile time (see
    /// `clients::repository`'s own doc comment on why), so a column-name
    /// typo in that statement would otherwise only ever surface the
    /// first time a real sync tick found something to refresh.
    ///
    /// Uses Highway 20's real, already-imported facility row (created
    /// during this same 2026-09-02 session's own live testing of
    /// `clients::create`) rather than inserting a fixture row: marks its
    /// `phone` as manually edited with an obviously-fake value, then
    /// refreshes from the real live run. Asserts the protected `phone`
    /// survived untouched while `name` (not protected) took the fresh
    /// value. Rolled back so this doesn't actually clobber the real row.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres AND a real Process Street API key -- see doc comment"]
    #[serial(client_pii_encryption_key_env)]
    async fn refresh_matching_facility_updates_unprotected_fields_and_skips_protected_ones() {
        let _ = dotenvy::from_filename(".env.local");

        let ps_config =
            ProcessStreetConfig::from_env().expect("PROCESS_STREET_API_KEY must be set in .env.local");
        let client = ProcessStreetClient::new(ps_config);

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let mut tx = begin_rls_transaction(&db, SYSTEM_USER_ID, &[SYSTEM_ROLE.to_string()])
            .await
            .expect("beginning an RLS transaction must succeed");

        // Highway 20's real Intake run id -- see this module's own live
        // tests above for the same constant used to find it via search.
        let run_id = "iy22NyiqGjwAAytKp0NErQ";

        let existing_id: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM clients.facilities WHERE ps_intake_run_id = $1")
                .bind(run_id)
                .fetch_optional(&mut *tx)
                .await
                .expect("facility lookup must succeed");
        let Some((facility_id,)) = existing_id else {
            tx.rollback().await.expect("rollback must succeed");
            panic!("Highway 20's facility row must already exist -- run clients::create's own live test first, or import it via the app");
        };

        // phone is seeded fake AND protected (must survive); name is
        // seeded stale but NOT protected (must be corrected back to the
        // real value) -- without a genuinely stale unprotected field,
        // the refreshed struct would equal current exactly and
        // `refresh_matching_facility` would correctly report no update
        // needed, proving nothing about the UPDATE statement itself.
        sqlx::query(
            "UPDATE clients.facilities SET phone = 'MANUALLY-CORRECTED', name = 'Stale Seeded Name', \
             manually_edited_fields = '{phone}' WHERE id = $1",
        )
        .bind(facility_id)
        .execute(&mut *tx)
        .await
        .expect("seeding the manually-edited phone and stale name must succeed");

        let fields = client
            .get_run_form_fields(run_id)
            .await
            .expect("fetching Highway 20's real fields must succeed");
        let mapped = map_intake_fields(&fields);

        let refreshed = refresh_matching_facility(&mut tx, run_id, &mapped.facility)
            .await
            .expect("refresh must succeed against the real schema");
        assert!(refreshed, "the fresh name should differ from the seeded state and trigger an update");

        let (phone, name): (Option<String>, String) =
            sqlx::query_as("SELECT phone, name FROM clients.facilities WHERE id = $1")
                .bind(facility_id)
                .fetch_one(&mut *tx)
                .await
                .expect("re-reading the facility must succeed");

        assert_eq!(phone.as_deref(), Some("MANUALLY-CORRECTED"), "a protected field must survive a refresh");
        assert_eq!(name, "Highway 20 Self Storage", "an unprotected field must take the fresh PS value");

        tx.rollback()
            .await
            .expect("rollback must succeed -- this is a one-time check, must not persist against the real row");
    }
}
