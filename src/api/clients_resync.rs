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

use std::collections::{HashMap, HashSet};

use axum::{
    extract::{Json, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::api::{internal_error, ApiErrorBody, AppState};
use crate::auth::{begin_rls_transaction, AuthenticatedUser};
use crate::client_ops::audit_log;
use crate::clients::create::diff_company_fields;
use crate::clients::intake_mapping::{map_intake_fields, MappedCompany, MappedFacility};
use crate::clients::sync::{
    apply_company_refresh, apply_facility_refresh, company_field_value, facility_field_value,
    facility_fields_that_differ,
};
use crate::process_street::FormField;

const PERMISSION: &str = "client_ops.perform";

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ApiErrorBody {
            error: "company_not_found",
            message: "No such company.".to_string(),
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
    let fetches = run_ids
        .into_iter()
        .map(|run_id| async move {
            let result = client.get_run_form_fields(&run_id).await;
            (run_id, result)
        });

    let mut fields_by_run_id = HashMap::new();
    for (run_id, result) in join_all(fetches).await {
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

async fn load_comparisons(
    tx: &mut Transaction<'_, Postgres>,
    client: &crate::process_street::ProcessStreetClient,
    company_id: Uuid,
) -> Result<Option<(CompanyComparison, Vec<FacilityComparison>)>, sqlx::Error> {
    let Some((company, facilities)) = fetch_company_and_facilities(tx, company_id).await? else {
        return Ok(None);
    };

    let mut run_ids: HashSet<String> = HashSet::new();
    if let Some(id) = &company.ps_intake_run_id {
        run_ids.insert(id.clone());
    }
    for facility in &facilities {
        if let Some(id) = &facility.ps_intake_run_id {
            run_ids.insert(id.clone());
        }
    }

    let fields_by_run_id = fetch_fresh_fields(client, run_ids).await;

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
            FacilityComparison { row: facility, fresh }
        })
        .collect();

    Ok(Some((
        CompanyComparison { row: company, fresh: company_fresh },
        facility_comparisons,
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
        if company.row.manually_edited_fields.iter().any(|p| p == field) {
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
        if facility.row.manually_edited_fields.iter().any(|p| p == field) {
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

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for resync preview");
            return internal_error("Could not preview the re-sync");
        }
    };

    let comparisons = match load_comparisons(&mut tx, &client, company_id).await {
        Ok(Some(comparisons)) => comparisons,
        Ok(None) => return not_found(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "resync preview query failed");
            return internal_error("Could not preview the re-sync");
        }
    };

    if let Err(err) = tx.commit().await {
        tracing::error!(error = %err, user_id = %user.user_id, "failed to commit resync preview transaction");
        return internal_error("Could not preview the re-sync");
    }

    let (company, facilities) = comparisons;
    let (mut safe_update_count, mut conflicts) = classify_company_diff(&company);
    for facility in &facilities {
        let (facility_safe, facility_conflicts) = classify_facility_diff(facility);
        safe_update_count += facility_safe;
        conflicts.extend(facility_conflicts);
    }

    Json(PreviewResyncResponse { safe_update_count, conflicts }).into_response()
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
            !resolutions
                .iter()
                .any(|r| r.use_fresh && r.entity_type == entity_type && r.entity_id == entity_id && &r.field == *field)
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

    let mut tx = match begin_rls_transaction(&state.db, user.user_id, &user.role_keys).await {
        Ok(tx) => tx,
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "failed to open transaction for resync apply");
            return internal_error("Could not apply the re-sync");
        }
    };

    let (company, facilities) = match load_comparisons(&mut tx, &client, company_id).await {
        Ok(Some(comparisons)) => comparisons,
        Ok(None) => return not_found(),
        Err(err) => {
            tracing::error!(error = %err, user_id = %user.user_id, "resync apply query failed");
            return internal_error("Could not apply the re-sync");
        }
    };

    let mut updated_count = 0;

    if let Some(fresh) = &company.fresh {
        let effective_protected =
            effective_protected_fields(&company.row.manually_edited_fields, &request.resolutions, "company", company.row.id);
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
        let Some(fresh) = &facility.fresh else { continue };
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
            "resolutions_applied": request.resolutions.iter().filter(|r| r.use_fresh).count(),
        }),
    )
    .await;

    Json(ApplyResyncResponse { updated_count }).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::{empty_state, onboarding_manager_user, test_user};

    fn company_row(legal_name: &str, manually_edited_fields: Vec<&str>) -> CompanyRow {
        CompanyRow {
            id: Uuid::new_v4(),
            ps_intake_run_id: Some("run-highway-20".to_string()),
            legal_name: legal_name.to_string(),
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
            manually_edited_fields: manually_edited_fields.into_iter().map(String::from).collect(),
        }
    }

    fn facility_row(name: &str, phone: &str, manually_edited_fields: Vec<&str>) -> FacilityRow {
        FacilityRow {
            id: Uuid::new_v4(),
            ps_intake_run_id: Some("run-facility".to_string()),
            name: name.to_string(),
            street_address: Some("1 Example St".to_string()),
            city: Some("Example City".to_string()),
            state: Some("IL".to_string()),
            zip: Some("60000".to_string()),
            phone: Some(phone.to_string()),
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
            website_url: Some("https://facility.example.com".to_string()),
            manually_edited_fields: manually_edited_fields.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn classify_company_diff_reports_no_conflicts_when_nothing_is_protected() {
        let row = company_row("Old Legal Name LLC", vec![]);
        let fresh = row.mapped();
        let fresh = MappedCompany { legal_name: Some("Prairie Enterprises LLC".to_string()), ..fresh };
        let comparison = CompanyComparison { row, fresh: Some(fresh) };

        let (safe_count, conflicts) = classify_company_diff(&comparison);

        assert_eq!(safe_count, 1);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn classify_company_diff_surfaces_a_conflict_for_a_protected_field_that_genuinely_differs() {
        let row = company_row("Manually Corrected LLC", vec!["legal_name"]);
        let fresh = row.mapped();
        let fresh = MappedCompany { legal_name: Some("Stale PS Legal Name LLC".to_string()), ..fresh };
        let comparison = CompanyComparison { row, fresh: Some(fresh) };

        let (safe_count, conflicts) = classify_company_diff(&comparison);

        assert_eq!(safe_count, 0);
        assert_eq!(conflicts.len(), 1);
        let conflict = &conflicts[0];
        assert_eq!(conflict.entity_type, "company");
        assert_eq!(conflict.field, "legal_name");
        assert_eq!(conflict.current_value.as_deref(), Some("Manually Corrected LLC"));
        assert_eq!(conflict.fresh_value.as_deref(), Some("Stale PS Legal Name LLC"));
    }

    #[test]
    fn classify_company_diff_reports_nothing_when_no_fresh_data_was_fetched() {
        // A run whose fetch failed (or has no ps_intake_run_id at all) --
        // `fresh: None` -- must never be reported as either a safe update
        // or a conflict; there's nothing to compare against.
        let row = company_row("Some Company LLC", vec!["legal_name"]);
        let comparison = CompanyComparison { row, fresh: None };

        let (safe_count, conflicts) = classify_company_diff(&comparison);

        assert_eq!(safe_count, 0);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn classify_company_diff_does_not_flag_a_protected_field_that_happens_to_already_match() {
        // A field can be manually edited yet coincidentally equal to
        // Process Street's current value -- that's not a conflict, since
        // there is nothing to choose between.
        let row = company_row("Same Value LLC", vec!["legal_name"]);
        let fresh = row.mapped();
        let comparison = CompanyComparison { row, fresh: Some(fresh) };

        let (safe_count, conflicts) = classify_company_diff(&comparison);

        assert_eq!(safe_count, 0);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn classify_facility_diff_reports_no_conflicts_when_nothing_is_protected() {
        let row = facility_row("Highway 20 Self Storage", "555-000-0000", vec![]);
        let fresh = row.mapped();
        let fresh = MappedFacility { phone: Some("555-111-1111".to_string()), ..fresh };
        let comparison = FacilityComparison { row, fresh: Some(fresh) };

        let (safe_count, conflicts) = classify_facility_diff(&comparison);

        assert_eq!(safe_count, 1);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn classify_facility_diff_surfaces_a_conflict_for_a_protected_field_that_genuinely_differs() {
        let row = facility_row("Highway 20 Self Storage", "555-CORRECTED", vec!["phone"]);
        let fresh = row.mapped();
        let fresh = MappedFacility { phone: Some("555-STALE".to_string()), ..fresh };
        let comparison = FacilityComparison { row, fresh: Some(fresh) };

        let (safe_count, conflicts) = classify_facility_diff(&comparison);

        assert_eq!(safe_count, 0);
        assert_eq!(conflicts.len(), 1);
        let conflict = &conflicts[0];
        assert_eq!(conflict.entity_type, "facility");
        assert_eq!(conflict.field, "phone");
        assert_eq!(conflict.current_value.as_deref(), Some("555-CORRECTED"));
        assert_eq!(conflict.fresh_value.as_deref(), Some("555-STALE"));
    }

    #[test]
    fn classify_facility_diff_never_reports_go_live_date() {
        // go_live_date isn't part of the comparison at all -- confirms a
        // facility whose go_live_date differs (which should never happen
        // since nothing ever writes a fresh value into it) still can't
        // surface as a phantom conflict or safe update.
        let row = facility_row("Highway 20 Self Storage", "555-000-0000", vec![]);
        let fresh = MappedFacility {
            go_live_date: chrono::NaiveDate::from_ymd_opt(2026, 12, 31),
            ..row.mapped()
        };
        let comparison = FacilityComparison { row, fresh: Some(fresh) };

        let (safe_count, conflicts) = classify_facility_diff(&comparison);

        assert_eq!(safe_count, 0);
        assert!(conflicts.is_empty());
    }

    #[test]
    fn company_field_value_reads_every_known_field_by_name() {
        let company = company_row("Prairie Enterprises LLC", vec![]).mapped();

        assert_eq!(company_field_value(&company, "legal_name").as_deref(), Some("Prairie Enterprises LLC"));
        assert_eq!(company_field_value(&company, "corporate_email").as_deref(), Some("office@example.com"));
        assert_eq!(company_field_value(&company, "not_a_real_field"), None);
    }

    #[test]
    fn facility_field_value_reads_every_known_field_by_name_including_numeric_ones() {
        let facility = facility_row("Highway 20 Self Storage", "555-000-0000", vec![]).mapped();

        assert_eq!(facility_field_value(&facility, "name").as_deref(), Some("Highway 20 Self Storage"));
        // units_count is Option<i32>, not Option<String> -- confirms it's
        // stringified, not silently dropped as a type mismatch.
        assert_eq!(facility_field_value(&facility, "units_count").as_deref(), Some("100"));
        assert_eq!(facility_field_value(&facility, "not_a_real_field"), None);
    }

    #[tokio::test]
    async fn preview_refuses_insufficient_permission_without_touching_anything() {
        let response = preview_resync(State(empty_state()), test_user(), Path(Uuid::new_v4())).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn preview_reports_not_configured_with_sufficient_permission() {
        let response = preview_resync(State(empty_state()), onboarding_manager_user(), Path(Uuid::new_v4())).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn apply_refuses_insufficient_permission_without_touching_anything() {
        let response = apply_resync(
            State(empty_state()),
            test_user(),
            Path(Uuid::new_v4()),
            Json(ApplyResyncRequest { resolutions: vec![] }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    fn conflict_resolution(entity_type: &str, entity_id: Uuid, field: &str, use_fresh: bool) -> ConflictResolution {
        ConflictResolution {
            entity_type: entity_type.to_string(),
            entity_id,
            field: field.to_string(),
            use_fresh,
        }
    }

    #[test]
    fn a_field_not_mentioned_in_any_resolution_stays_protected() {
        let stored = vec!["legal_name".to_string()];
        let entity_id = Uuid::new_v4();

        let effective = effective_protected_fields(&stored, &[], "company", entity_id);

        assert_eq!(effective, stored);
    }

    #[test]
    fn a_resolution_with_use_fresh_false_keeps_the_field_protected() {
        let stored = vec!["legal_name".to_string()];
        let entity_id = Uuid::new_v4();
        let resolutions = vec![conflict_resolution("company", entity_id, "legal_name", false)];

        let effective = effective_protected_fields(&stored, &resolutions, "company", entity_id);

        assert_eq!(effective, stored);
    }

    #[test]
    fn a_resolution_with_use_fresh_true_drops_the_field_from_protection() {
        let stored = vec!["legal_name".to_string(), "corporate_phone".to_string()];
        let entity_id = Uuid::new_v4();
        let resolutions = vec![conflict_resolution("company", entity_id, "legal_name", true)];

        let effective = effective_protected_fields(&stored, &resolutions, "company", entity_id);

        assert_eq!(effective, vec!["corporate_phone".to_string()]);
    }

    #[test]
    fn a_resolution_for_a_different_entity_id_does_not_affect_this_one() {
        // Two facilities can each have their own "phone" conflict --
        // resolving one must never accidentally clear the other's.
        let stored = vec!["phone".to_string()];
        let this_facility = Uuid::new_v4();
        let other_facility = Uuid::new_v4();
        let resolutions = vec![conflict_resolution("facility", other_facility, "phone", true)];

        let effective = effective_protected_fields(&stored, &resolutions, "facility", this_facility);

        assert_eq!(effective, stored);
    }

    #[test]
    fn a_resolution_for_a_different_entity_type_with_the_same_id_does_not_affect_this_one() {
        // Belt-and-suspenders: entity_type must be checked too, not just
        // entity_id, even though a company and a facility never actually
        // share a UUID in practice.
        let stored = vec!["subdomain".to_string()];
        let id = Uuid::new_v4();
        let resolutions = vec![conflict_resolution("facility", id, "subdomain", true)];

        let effective = effective_protected_fields(&stored, &resolutions, "company", id);

        assert_eq!(effective, stored);
    }
}
