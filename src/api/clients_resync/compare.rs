//! Comparing stored data with Process Street's current values: the comparison types, the short-lived preview cache and the per-field conflict classification.

use super::fetch::{
    fetch_company_and_facilities, fetch_fresh_fields, fetch_fresh_merchant_account_data,
    fetch_linked_merchant_account_runs, MerchantAccountRefresh,
};
use super::rows::{CompanyRow, FacilityRow};
use crate::auth::begin_rls_transaction;
use crate::clients::create::diff_company_fields;
use crate::clients::intake_mapping::{map_intake_fields, MappedCompany, MappedFacility};
use crate::clients::person_index::{extract_intake_people, ExtractedPerson};
use crate::clients::ps_task_roles;
use crate::clients::sync::{
    company_field_value, facility_field_value, facility_fields_that_differ,
};
use crate::process_street::FormField;
use parking_lot::RwLock;
use serde::Serialize;
use sqlx::{Postgres, Transaction};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
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

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
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
pub(super) struct CompanyComparison {
    pub(super) row: CompanyRow,
    pub(super) fresh: Option<MappedCompany>,
}

pub(super) struct FacilityComparison {
    pub(super) row: FacilityRow,
    pub(super) fresh: Option<MappedFacility>,
}

/// What one `load_comparisons` call produces -- named so the cache
/// below and `load_comparisons`'s own return type don't each spell out
/// the same four-tuple.
pub(super) type Comparisons = (
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
pub(super) const PREVIEW_CACHE_TTL: Duration = Duration::from_secs(300);

pub(crate) struct CachedComparisons {
    pub(super) computed_at: Instant,
    pub(super) comparisons: Comparisons,
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
pub(super) fn usable_cache_entry(entry: Option<CachedComparisons>) -> Option<Comparisons> {
    entry
        .filter(|cached| cached.computed_at.elapsed() < PREVIEW_CACHE_TTL)
        .map(|cached| cached.comparisons)
}

/// What the database half of a comparison needs from Process Street's
/// half: the rows as they are right now, plus which runs to fetch.
pub(super) struct ComparisonInputs {
    pub(super) company: CompanyRow,
    pub(super) facilities: Vec<FacilityRow>,
    pub(super) merchant_account_run_ids: HashMap<Uuid, String>,
    /// `ps_task_roles::QMS_CREDENTIALS_ROLE`'s mapped task names, read
    /// with the rest of the database half so the PS half stays DB-free.
    pub(super) qms_credential_task_names: Vec<String>,
}

/// The database-only half of building a comparison. Runs inside the
/// caller's transaction, which must be committed BEFORE
/// `fetch_ps_data` starts -- see `load_comparisons`.
pub(super) async fn read_comparison_inputs(
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
pub(super) async fn fetch_ps_data(
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
pub(super) fn assemble_comparisons(
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
pub(super) async fn load_comparisons(
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
pub(super) fn classify_company_diff(company: &CompanyComparison) -> (usize, Vec<ResyncConflict>) {
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
pub(super) fn classify_facility_diff(
    facility: &FacilityComparison,
) -> (usize, Vec<ResyncConflict>) {
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
