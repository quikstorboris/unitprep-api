//! Scoped manual "Re-sync" for one already-imported client -- lets a
//! manager re-pull that company's own source run plus every one of its
//! facilities' own runs from Process Street right now, without waiting
//! for the next scheduled interval (see `clients::sync`'s own module
//! doc on why that interval can now be much shorter than the old
//! once-daily default, but still isn't "immediately").
//!
//! Two-phase, matching the hybrid design Boris asked for (2026-09-02):
//! a field that's been manually corrected in OO (`manually_edited_fields`,
//! same mechanism the scheduled sync silently respects) never gets
//! silently overwritten here either -- but unlike the scheduled sync,
//! a human is actually watching this one, so a real conflict (the field
//! is protected AND Process Street's current value genuinely differs)
//! is surfaced for an explicit per-field choice instead of always just
//! skipping it.
//!
//! `preview_resync` reports what would happen without writing anything;
//! `apply_resync` takes the caller's own resolutions for whichever
//! conflicts they want to overwrite from Process Street (unlisted or
//! `use_fresh: false` conflicts keep the manually-set value, same as the
//! scheduled sync's own default) and writes.
//!
//! **`apply_resync` reuses `preview_resync`'s own fetch, not a second
//! one** (2026-09-23, against Boris's own observation that confirming a
//! preview -- even choosing to keep every field as-is -- still took as
//! long as the preview itself). Both phases were independently calling
//! `load_comparisons`, which does the expensive part twice: every
//! linked run's fields *and* tasks, fetched live from Process Street,
//! for the company plus every one of its facilities. `preview_resync`
//! now stashes its own `load_comparisons` result in
//! `AppState::resync_preview_cache`, keyed by `company_id`; `apply_resync`
//! drains that entry (single-use -- a second apply without a fresh
//! preview falls through to fetching live again, same as a cache miss)
//! when it's still within `PREVIEW_CACHE_TTL`, and only calls
//! `load_comparisons` itself when there's nothing usable there. A
//! missing or stale entry is always a safe fallback to today's
//! behavior, never a correctness risk -- this is purely cutting a
//! redundant round trip to Process Street (and the PS API-rate-limit
//! cost that comes with it) out of the common path.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::integrations::http::join_all_bounded;
use axum::{
    extract::{Json, Path, State},
    response::{IntoResponse, Response},
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::api::{
    encryption_not_configured, internal_error, not_found, process_street_not_configured, AppState,
};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log;
use crate::clients::create::diff_company_fields;
use crate::clients::intake_mapping::{map_intake_fields, MappedCompany, MappedFacility};
use crate::clients::merchant_account_mapping::{
    credentials_added_to_qms_from_tasks, map_merchant_account_fields, MappedMerchantAccount,
};
use crate::clients::person_index::{extract_intake_people, ExtractedPerson};
use crate::clients::ps_task_roles;
use crate::clients::repository::{
    resync_merchant_account_run, upsert_task_status, IngestMerchantAccountError,
};
use crate::clients::sync::{
    apply_company_refresh, apply_facility_refresh, company_field_value, facility_field_value,
    facility_fields_that_differ,
};
use crate::process_street::{FormField, Task};

const PERMISSION: &str = "client_ops.perform";

#[derive(sqlx::FromRow)]
struct CompanyRow {
    id: Uuid,
    ps_intake_run_id: Option<String>,
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

impl CompanyRow {
    fn mapped(&self) -> MappedCompany {
        MappedCompany {
            legal_name: Some(self.legal_name.clone()),
            corporate_email: self.corporate_email.clone(),
            corporate_phone: self.corporate_phone.clone(),
            corporate_address_street: self.corporate_address_street.clone(),
            corporate_address_city: self.corporate_address_city.clone(),
            corporate_address_state: self.corporate_address_state.clone(),
            corporate_address_zip: self.corporate_address_zip.clone(),
            subdomain: self.subdomain.clone(),
            accepted_payment_methods: self.accepted_payment_methods.clone(),
            accounting_basis: self.accounting_basis.clone(),
            payment_scheme: self.payment_scheme.clone(),
            offers_tenant_insurance_raw: self.offers_tenant_insurance_raw.clone(),
            insurance_provider: self.insurance_provider.clone(),
            website_url: self.website_url.clone(),
        }
    }
}

#[derive(sqlx::FromRow)]
struct FacilityRow {
    id: Uuid,
    ps_intake_run_id: Option<String>,
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

impl FacilityRow {
    fn mapped(&self) -> MappedFacility {
        MappedFacility {
            name: Some(self.name.clone()),
            street_address: self.street_address.clone(),
            city: self.city.clone(),
            state: self.state.clone(),
            zip: self.zip.clone(),
            phone: self.phone.clone(),
            email: self.email.clone(),
            units_count: self.units_count,
            primary_storage_offering: self.primary_storage_offering.clone(),
            previous_pms: self.previous_pms.clone(),
            access_control_system: self.access_control_system.clone(),
            go_live_date: self.go_live_date,
            dropbox_folder_url: self.dropbox_folder_url.clone(),
            subdomain: self.subdomain.clone(),
            subdomain_exists_in_qms_raw: self.subdomain_exists_in_qms_raw.clone(),
            system_email: self.system_email.clone(),
            website_url: self.website_url.clone(),
        }
    }
}

async fn fetch_company_and_facilities(
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
async fn fetch_fresh_fields(
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
struct MerchantAccountRefresh {
    ps_new_merchant_run_id: String,
    mapped: MappedMerchantAccount,
    credentials_added_to_qms: bool,
    tasks: Vec<Task>,
}

/// `clients.facility_merchant_accounts.ps_new_merchant_run_id` for every
/// one of this company's facilities that's actually linked to Elavon --
/// a facility with no row there (never linked) is simply absent from the
/// result, same "nothing to refresh against" resilience `fetch_fresh_fields`
/// already has for a missing `ps_intake_run_id`.
async fn fetch_linked_merchant_account_runs(
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
async fn fetch_fresh_merchant_account_data(
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

#[derive(Debug, Serialize)]
pub struct ResyncConflict {
    /// "company" | "facility".
    pub entity_type: &'static str,
    pub entity_id: Uuid,
    /// e.g. the company's legal name or the facility's name -- so the
    /// confirmation UI can label a conflict without a second lookup.
    pub entity_label: String,
    pub field: String,
    pub current_value: Option<String>,
    pub fresh_value: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PreviewResyncResponse {
    /// How many fields would update automatically -- not manually
    /// edited, so no choice is needed.
    pub safe_update_count: usize,
    pub conflicts: Vec<ResyncConflict>,
    /// How many linked facilities' Elavon/Merchant Account data (task
    /// checklist + `credentials_added_to_qms`, financials, parties) will
    /// be refreshed -- always a full overwrite, so unlike the counts
    /// above there is no per-field conflict to list.
    pub merchant_accounts_to_refresh: usize,
}

/// One company/facility's own (current, fresh) pair plus its protected
/// set, resolved once and shared between `preview_resync` and
/// `apply_resync` so the two can't drift in how they classify a field.
struct CompanyComparison {
    row: CompanyRow,
    fresh: Option<MappedCompany>,
}

struct FacilityComparison {
    row: FacilityRow,
    fresh: Option<MappedFacility>,
}

/// What one `load_comparisons` call produces -- named so the cache
/// below and `load_comparisons`'s own return type don't each spell out
/// the same four-tuple.
type Comparisons = (
    CompanyComparison,
    Vec<FacilityComparison>,
    HashMap<String, Vec<ExtractedPerson>>,
    HashMap<Uuid, MerchantAccountRefresh>,
);

/// How long a preview's fetched-from-PS snapshot stays valid for a
/// follow-up apply to reuse -- long enough to cover "reviewed the
/// conflicts, picked resolutions, clicked confirm" (a human round
/// trip, seconds to a couple minutes), short enough that a tab left
/// open a long time before confirming falls back to a fresh fetch
/// rather than applying a stale one.
const PREVIEW_CACHE_TTL: Duration = Duration::from_secs(300);

pub(crate) struct CachedComparisons {
    computed_at: Instant,
    comparisons: Comparisons,
}

/// See this module's own doc comment for why `apply_resync` reuses
/// `preview_resync`'s fetch instead of repeating it. Keyed by
/// `company_id` -- only one preview per company is ever worth keeping,
/// so a second preview for the same company simply overwrites the
/// first rather than accumulating entries.
pub type ResyncPreviewCache = Arc<RwLock<HashMap<Uuid, CachedComparisons>>>;

/// `None` for both a missing entry and one older than `PREVIEW_CACHE_TTL`
/// -- pulled out of `apply_resync` as its own pure function so the TTL
/// boundary is unit-testable without a database or a live Process
/// Street client.
fn usable_cache_entry(entry: Option<CachedComparisons>) -> Option<Comparisons> {
    entry
        .filter(|cached| cached.computed_at.elapsed() < PREVIEW_CACHE_TTL)
        .map(|cached| cached.comparisons)
}

/// What the database half of a comparison needs from Process Street's
/// half: the rows as they are right now, plus which runs to fetch.
struct ComparisonInputs {
    company: CompanyRow,
    facilities: Vec<FacilityRow>,
    merchant_account_run_ids: HashMap<Uuid, String>,
    /// `ps_task_roles::QMS_CREDENTIALS_ROLE`'s mapped task names, read
    /// with the rest of the database half so the PS half stays DB-free.
    qms_credential_task_names: Vec<String>,
}

/// The database-only half of building a comparison. Runs inside the
/// caller's transaction, which must be committed BEFORE
/// `fetch_ps_data` starts -- see `load_comparisons`.
async fn read_comparison_inputs(
    tx: &mut Transaction<'_, Postgres>,
    company_id: Uuid,
) -> Result<Option<ComparisonInputs>, sqlx::Error> {
    let Some((company, facilities)) = fetch_company_and_facilities(tx, company_id).await? else {
        return Ok(None);
    };

    let facility_ids: Vec<Uuid> = facilities.iter().map(|f| f.id).collect();
    let merchant_account_run_ids = fetch_linked_merchant_account_runs(tx, &facility_ids).await?;
    let qms_credential_task_names =
        ps_task_roles::load_task_names(tx, ps_task_roles::QMS_CREDENTIALS_ROLE).await?;

    Ok(Some(ComparisonInputs {
        company,
        facilities,
        merchant_account_run_ids,
        qms_credential_task_names,
    }))
}

/// The Process Street-only half: fetches every distinct intake run the
/// company and its facilities cite, plus each linked Merchant Account
/// run's fields and tasks, all concurrently. Touches no database, so no
/// transaction (and no pooled connection) is held while it waits on the
/// network.
async fn fetch_ps_data(
    client: &crate::process_street::ProcessStreetClient,
    inputs: &ComparisonInputs,
) -> (
    HashMap<String, Vec<FormField>>,
    HashMap<Uuid, MerchantAccountRefresh>,
) {
    let mut run_ids: HashSet<String> = HashSet::new();
    if let Some(id) = &inputs.company.ps_intake_run_id {
        run_ids.insert(id.clone());
    }
    for facility in &inputs.facilities {
        if let Some(id) = &facility.ps_intake_run_id {
            run_ids.insert(id.clone());
        }
    }

    tokio::join!(
        fetch_fresh_fields(client, run_ids),
        fetch_fresh_merchant_account_data(
            client,
            inputs.merchant_account_run_ids.clone(),
            &inputs.qms_credential_task_names,
        )
    )
}

/// The pure half: pairs each database row with its freshly-fetched
/// Process Street counterpart.
fn assemble_comparisons(
    inputs: ComparisonInputs,
    fields_by_run_id: HashMap<String, Vec<FormField>>,
    merchant_account_refreshes: HashMap<Uuid, MerchantAccountRefresh>,
) -> Comparisons {
    let ComparisonInputs {
        company,
        facilities,
        ..
    } = inputs;

    // Same `extract_intake_people` projection the scheduled/"Sync Now"
    // background sync writes into `clients.ps_person_index` (see
    // `clients::sync::orchestrator::apply_fetched_runs`) -- this per-client
    // "Re-sync" button fetches these same runs' fields anyway for the
    // company/facility field refresh below, so it can keep the Users
    // tab's own "Add User" candidates fresh too, at no extra PS request
    // cost. Without this, a person added in Process Street after a
    // facility's already been imported into OO never shows up here no
    // matter how many times Re-sync is clicked -- only the separate
    // scheduled sync (or "Sync Now" on the search page) ever refreshed
    // `ps_person_index` before this fix.
    let people_by_run_id: HashMap<String, Vec<ExtractedPerson>> = fields_by_run_id
        .iter()
        .map(|(run_id, fields)| (run_id.clone(), extract_intake_people(fields)))
        .collect();

    let company_fresh = company
        .ps_intake_run_id
        .as_deref()
        .and_then(|id| fields_by_run_id.get(id))
        .map(|fields| map_intake_fields(fields).company);

    let facility_comparisons = facilities
        .into_iter()
        .map(|facility| {
            let fresh = facility
                .ps_intake_run_id
                .as_deref()
                .and_then(|id| fields_by_run_id.get(id))
                .map(|fields| map_intake_fields(fields).facility);
            FacilityComparison {
                row: facility,
                fresh,
            }
        })
        .collect();

    (
        CompanyComparison {
            row: company,
            fresh: company_fresh,
        },
        facility_comparisons,
        people_by_run_id,
        merchant_account_refreshes,
    )
}

/// Builds a comparison in three phases, in this order, so that **no
/// database transaction is open while Process Street is being called**:
/// a short read transaction (committed before it returns), then the
/// network fetch, then pure assembly.
///
/// This used to run the whole thing inside the caller's transaction. The
/// network phase fetches every cited run's fields and tasks (seconds for
/// a company with many facilities), and a pooled Neon connection sat
/// idle inside an open transaction for all of it -- one of only 20, and
/// one more for every concurrent Re-sync.
async fn load_comparisons(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    client: &crate::process_street::ProcessStreetClient,
    company_id: Uuid,
) -> Result<Option<Comparisons>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let inputs = read_comparison_inputs(&mut tx, company_id).await?;
    tx.commit().await?;

    let Some(inputs) = inputs else {
        return Ok(None);
    };

    let (fields_by_run_id, merchant_account_refreshes) = fetch_ps_data(client, &inputs).await;

    Ok(Some(assemble_comparisons(
        inputs,
        fields_by_run_id,
        merchant_account_refreshes,
    )))
}

/// Splits the fields where `fresh` differs from `current` into "safe"
/// (not manually edited -- `apply_company_refresh`/`apply_facility_refresh`
/// will update it silently) and "conflicts" (manually edited AND
/// genuinely different -- needs the caller's own choice). Shared by
/// `preview_resync` (to report) and `apply_resync` (to know which
/// resolutions it actually needs).
fn classify_company_diff(company: &CompanyComparison) -> (usize, Vec<ResyncConflict>) {
    let Some(fresh) = &company.fresh else {
        return (0, Vec::new());
    };
    let current = company.row.mapped();
    let differing = diff_company_fields(fresh, &current);

    let mut safe_count = 0;
    let mut conflicts = Vec::new();
    for field in differing {
        if company
            .row
            .manually_edited_fields
            .iter()
            .any(|p| p == field)
        {
            conflicts.push(ResyncConflict {
                entity_type: "company",
                entity_id: company.row.id,
                entity_label: company.row.legal_name.clone(),
                field: field.to_string(),
                current_value: company_field_value(&current, field),
                fresh_value: company_field_value(fresh, field),
            });
        } else {
            safe_count += 1;
        }
    }
    (safe_count, conflicts)
}

/// `classify_company_diff`'s counterpart for one facility.
fn classify_facility_diff(facility: &FacilityComparison) -> (usize, Vec<ResyncConflict>) {
    let Some(fresh) = &facility.fresh else {
        return (0, Vec::new());
    };
    let current = facility.row.mapped();
    let differing = facility_fields_that_differ(fresh, &current);

    let mut safe_count = 0;
    let mut conflicts = Vec::new();
    for field in differing {
        if facility
            .row
            .manually_edited_fields
            .iter()
            .any(|p| p == field)
        {
            conflicts.push(ResyncConflict {
                entity_type: "facility",
                entity_id: facility.row.id,
                entity_label: facility.row.name.clone(),
                field: field.to_string(),
                current_value: facility_field_value(&current, field),
                fresh_value: facility_field_value(fresh, field),
            });
        } else {
            safe_count += 1;
        }
    }
    (safe_count, conflicts)
}

/// Requires `client_ops.perform` -- same gate `create_client` uses; this
/// reads live PS data but writes nothing, still gated the same way since
/// it's part of the same client-ops action, not a plain read.
pub async fn preview_resync(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "preview_resync", None, None)
        .await
    {
        return response;
    }

    let Some(client) = state.process_street.clone() else {
        return process_street_not_configured();
    };

    let comparisons = match load_comparisons(
        &state.db,
        user.user_id,
        &user.role_keys,
        &client,
        company_id,
    )
    .await
    {
        Ok(Some(comparisons)) => comparisons,
        Ok(None) => return not_found("company_not_found", "No such company.".to_string()),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "resync preview query failed");
            return internal_error("Could not preview the re-sync");
        }
    };

    let (company, facilities, people_by_run_id, merchant_account_refreshes) = comparisons;
    let (mut safe_update_count, mut conflicts) = classify_company_diff(&company);
    for facility in &facilities {
        let (facility_safe, facility_conflicts) = classify_facility_diff(facility);
        safe_update_count += facility_safe;
        conflicts.extend(facility_conflicts);
    }

    let merchant_accounts_to_refresh = merchant_account_refreshes.len();

    // Stashed for `apply_resync` to reuse -- see this module's own doc
    // comment. Overwrites any still-unused entry from an earlier
    // preview of this same company, which is exactly right: this is
    // the freshest fetch, so it's the one a follow-up apply should act on.
    state.resync_preview_cache.write().insert(
        company_id,
        CachedComparisons {
            computed_at: Instant::now(),
            comparisons: (
                company,
                facilities,
                people_by_run_id,
                merchant_account_refreshes,
            ),
        },
    );

    Json(PreviewResyncResponse {
        safe_update_count,
        conflicts,
        merchant_accounts_to_refresh,
    })
    .into_response()
}

#[derive(Debug, Deserialize)]
pub struct ConflictResolution {
    pub entity_type: String,
    pub entity_id: Uuid,
    pub field: String,
    /// `true` overwrites this one field from Process Street (and clears
    /// it from `manually_edited_fields`, since it no longer diverges);
    /// `false` -- or simply not listing this conflict at all -- keeps
    /// the manually-set value, same as the scheduled sync's own default.
    pub use_fresh: bool,
}

#[derive(Debug, Deserialize)]
pub struct ApplyResyncRequest {
    #[serde(default)]
    pub resolutions: Vec<ConflictResolution>,
}

#[derive(Debug, Serialize)]
pub struct ApplyResyncResponse {
    pub updated_count: usize,
    pub merchant_accounts_refreshed: usize,
}

/// The fields still protected after folding in this apply's own
/// resolutions -- a field resolved `use_fresh: true` for this exact
/// entity is dropped from the protected set (it no longer diverges from
/// Process Street); everything else stays exactly as stored.
fn effective_protected_fields(
    stored: &[String],
    resolutions: &[ConflictResolution],
    entity_type: &str,
    entity_id: Uuid,
) -> Vec<String> {
    stored
        .iter()
        .filter(|field| {
            !resolutions.iter().any(|r| {
                r.use_fresh
                    && r.entity_type == entity_type
                    && r.entity_id == entity_id
                    && &r.field == *field
            })
        })
        .cloned()
        .collect()
}

pub async fn apply_resync(
    State(state): State<AppState>,
    user: AuthenticatedUser,
    Path(company_id): Path<Uuid>,
    Json(request): Json<ApplyResyncRequest>,
) -> Response {
    if let Err(response) = user
        .require_permission(&state.db, PERMISSION, "apply_resync", None, None)
        .await
    {
        return response;
    }

    let Some(client) = state.process_street.clone() else {
        return process_street_not_configured();
    };

    // Single-use: a hit is consumed here whether or not it's still
    // fresh enough to use, so a stale leftover never lingers to be
    // mistaken for a later preview's own result.
    let cached = usable_cache_entry(state.resync_preview_cache.write().remove(&company_id));

    // On a miss the comparison is built HERE, before the write
    // transaction opens: `load_comparisons` reads the rows in its own
    // short transaction, calls Process Street with no transaction held,
    // and only then does the write transaction below begin. (This used to
    // open the write transaction first and call Process Street inside it.)
    // The rows it read are then a few seconds old by the time the writes
    // run -- the same staleness window the preview-cache path above has
    // always had, for up to `PREVIEW_CACHE_TTL`.
    let (company, facilities, people_by_run_id, merchant_account_refreshes) = match cached {
        Some(comparisons) => comparisons,
        None => match load_comparisons(
            &state.db,
            user.user_id,
            &user.role_keys,
            &client,
            company_id,
        )
        .await
        {
            Ok(Some(comparisons)) => comparisons,
            Ok(None) => return not_found("company_not_found", "No such company.".to_string()),
            Err(err) => {
                tracing::error!(error = %err, user_id = %user.user_id, "resync apply query failed");
                return internal_error("Could not apply the re-sync");
            }
        },
    };

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for resync apply");
            return internal_error("Could not apply the re-sync");
        }
    };

    let mut updated_count = 0;

    if let Some(fresh) = &company.fresh {
        let effective_protected = effective_protected_fields(
            &company.row.manually_edited_fields,
            &request.resolutions,
            "company",
            company.row.id,
        );
        let current = company.row.mapped();
        let refreshed = apply_company_refresh(&current, fresh, &effective_protected);

        if refreshed != current || effective_protected != company.row.manually_edited_fields {
            let legal_name = refreshed
                .legal_name
                .as_deref()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("(unnamed company)");

            let result = sqlx::query(
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
            .execute(&mut *tx)
            .await;

            if let Err(err) = result {
                tracing::error!(error = %err, user_id = %user.user_id, "resync apply failed to update company");
                let _ = tx.rollback().await;
                return internal_error("Could not apply the re-sync");
            }
            updated_count += 1;
        }
    }

    for facility in &facilities {
        let Some(fresh) = &facility.fresh else {
            continue;
        };
        let effective_protected = effective_protected_fields(
            &facility.row.manually_edited_fields,
            &request.resolutions,
            "facility",
            facility.row.id,
        );
        let current = facility.row.mapped();
        let refreshed = apply_facility_refresh(&current, fresh, &effective_protected);

        if refreshed != current || effective_protected != facility.row.manually_edited_fields {
            let result = sqlx::query(
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
            .execute(&mut *tx)
            .await;

            if let Err(err) = result {
                tracing::error!(error = %err, user_id = %user.user_id, "resync apply failed to update a facility");
                let _ = tx.rollback().await;
                return internal_error("Could not apply the re-sync");
            }
            updated_count += 1;
        }
    }

    // Refresh `clients.ps_person_index` for every run just fetched --
    // the Users tab's own "Add User" candidates (`api::clients_facility_people`)
    // are sourced entirely from that table, and until this it was only
    // ever kept fresh by the separate scheduled/"Sync Now" background
    // sync, never by this per-client button despite its own doc comment
    // claiming a full re-pull. Same rebuild-wholesale, delete-then-insert
    // shape `apply_fetched_runs` already uses -- a genuine `run_name` value is
    // only available from `ps_sync_state` (that data isn't part of the
    // form-fields fetch this endpoint already makes), so a run with no
    // prior sync_state row at all falls back to the entity's own current
    // name rather than leaving `run_name` unset (NOT NULL).
    let mut run_names: HashMap<String, String> = sqlx::query_as::<_, (String, String)>(
        "SELECT ps_run_id, run_name FROM clients.ps_sync_state \
         WHERE workflow = 'intake' AND ps_run_id = ANY($1)",
    )
    .bind(people_by_run_id.keys().cloned().collect::<Vec<_>>())
    .fetch_all(&mut *tx)
    .await
    .unwrap_or_default()
    .into_iter()
    .collect();

    if let Some(run_id) = &company.row.ps_intake_run_id {
        run_names
            .entry(run_id.clone())
            .or_insert_with(|| company.row.legal_name.clone());
    }
    for facility in &facilities {
        if let Some(run_id) = &facility.row.ps_intake_run_id {
            run_names
                .entry(run_id.clone())
                .or_insert_with(|| facility.row.name.clone());
        }
    }

    // One DELETE for every refreshed run, then one multi-row INSERT -- two
    // statements however many runs and people there are. This used to be a
    // DELETE per run and an INSERT per person, each its own round trip
    // inside this transaction: a company with ten runs and a few people
    // each paid sixty-odd network round trips here before committing.
    let refreshed_run_ids: Vec<String> = people_by_run_id.keys().cloned().collect();

    if let Err(err) = sqlx::query(
        "DELETE FROM clients.ps_person_index WHERE workflow = 'intake' AND ps_run_id = ANY($1)",
    )
    .bind(&refreshed_run_ids)
    .execute(&mut *tx)
    .await
    {
        tracing::error!(error = %err, user_id = %user.user_id, "resync apply failed to clear ps_person_index");
        let _ = tx.rollback().await;
        return internal_error("Could not apply the re-sync");
    }

    let mut index_run_ids: Vec<&str> = Vec::new();
    let mut index_run_names: Vec<&str> = Vec::new();
    let mut index_full_names: Vec<&str> = Vec::new();
    let mut index_emails: Vec<Option<&str>> = Vec::new();
    let mut index_phones: Vec<Option<&str>> = Vec::new();
    let mut index_roles: Vec<&str> = Vec::new();
    for (run_id, people) in &people_by_run_id {
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
        if let Err(err) = sqlx::query(
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
        .execute(&mut *tx)
        .await
        {
            tracing::error!(error = %err, user_id = %user.user_id, "resync apply failed to index people");
            let _ = tx.rollback().await;
            return internal_error("Could not apply the re-sync");
        }
    }

    // Refresh every linked facility's Elavon/Merchant Account picture --
    // see `MerchantAccountRefresh`'s own doc comment for why this button
    // never touched this data before. Always a full overwrite (no
    // manual-edit protection on this tab, same as
    // `resync_elavon_data`), so unlike the company/facility loops above
    // there's no protected-field comparison first.
    let mut merchant_accounts_refreshed = 0;
    for (facility_id, refresh) in &merchant_account_refreshes {
        if let Err(err) = resync_merchant_account_run(
            &mut tx,
            *facility_id,
            &refresh.mapped,
            &refresh.ps_new_merchant_run_id,
            refresh.credentials_added_to_qms,
        )
        .await
        {
            tracing::error!(error = %err, user_id = %user.user_id, facility_id = %facility_id, "resync apply failed to refresh a facility's Merchant Account data");
            let _ = tx.rollback().await;
            return match err {
                IngestMerchantAccountError::Encryption(_) => encryption_not_configured(),
                IngestMerchantAccountError::Database(_) => {
                    internal_error("Could not apply the re-sync")
                }
            };
        }

        if let Err(err) =
            upsert_task_status(&mut tx, *facility_id, "merchant_account", &refresh.tasks).await
        {
            tracing::error!(error = %err, user_id = %user.user_id, facility_id = %facility_id, "resync apply failed to refresh a facility's Merchant Account task statuses");
            let _ = tx.rollback().await;
            return internal_error("Could not apply the re-sync");
        }

        merchant_accounts_refreshed += 1;
    }

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit resync apply transaction");
        return internal_error("Could not apply the re-sync");
    }

    audit_log::record(
        &state.db,
        audit_log::event::SYNC_COMPLETED,
        user.user_id,
        "company",
        Some(&company_id.to_string()),
        audit_log::Change::none(),
        None,
        None,
        serde_json::json!({
            "trigger": "manual_resync",
            "updated_count": updated_count,
            "people_indexed": people_indexed,
            "merchant_accounts_refreshed": merchant_accounts_refreshed,
            "resolutions_applied": request.resolutions.iter().filter(|r| r.use_fresh).count(),
        }),
    )
    .await;

    Json(ApplyResyncResponse {
        updated_count,
        merchant_accounts_refreshed,
    })
    .into_response()
}

#[cfg(test)]
#[path = "clients_resync_tests.rs"]
mod tests;
