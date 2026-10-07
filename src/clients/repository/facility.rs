//! Inserting a company, its facilities and their policy rows.

use super::people::link_person_to_facility;
use crate::clients::intake_mapping::{MappedCompany, MappedFacility, MappedIntakeRun};
use crate::clients::people::{ParsedPerson, PersonAssignment};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Inserts just the company row -- `legal_name` is passed explicitly
/// rather than read off `MappedIntakeRun` directly so the caller
/// decides the final name (see `clients::company_naming::resolve_company_name`,
/// which needs Merchant Account data this module never sees) rather
/// than baking name resolution into the write layer. Used both by
/// `ingest_intake_run` below (one run, always creates its own company)
/// and by the "Add to OO" split-creation flow, where exactly one
/// selected run is designated the company source and the rest attach
/// to it as facilities -- see `clients::create`.
pub async fn insert_company(
    tx: &mut Transaction<'_, Postgres>,
    legal_name: &str,
    company: &MappedCompany,
    ps_intake_run_id: &str,
    raw_ps_snapshot: &Value,
    manually_edited_fields: &[&str],
) -> Result<Uuid, sqlx::Error> {
    let (company_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.companies
            (legal_name, corporate_email, corporate_phone, corporate_address_street,
             corporate_address_city, corporate_address_state, corporate_address_zip,
             subdomain, accepted_payment_methods, accounting_basis, payment_scheme,
             offers_tenant_insurance_raw, insurance_provider, website_url,
             source, ps_intake_run_id, raw_ps_snapshot, manually_edited_fields, last_synced_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 'process_street', $15, $16, $17, now())
         RETURNING id",
    )
    .bind(legal_name)
    .bind(&company.corporate_email)
    .bind(&company.corporate_phone)
    .bind(&company.corporate_address_street)
    .bind(&company.corporate_address_city)
    .bind(&company.corporate_address_state)
    .bind(&company.corporate_address_zip)
    .bind(&company.subdomain)
    .bind(&company.accepted_payment_methods)
    .bind(&company.accounting_basis)
    .bind(&company.payment_scheme)
    .bind(&company.offers_tenant_insurance_raw)
    .bind(&company.insurance_provider)
    .bind(&company.website_url)
    .bind(ps_intake_run_id)
    .bind(raw_ps_snapshot)
    .bind(manually_edited_fields)
    .fetch_one(&mut **tx)
    .await?;

    Ok(company_id)
}

/// Inserts a facility attached to an already-existing `company_id` --
/// never creates a company itself. Shared by `ingest_intake_run` (the
/// company it attaches to was just created by `insert_company` above,
/// same call) and the split-creation flow (attaches to a company
/// created from a *different* run entirely, or one that already
/// existed before this batch).
pub async fn insert_facility(
    tx: &mut Transaction<'_, Postgres>,
    company_id: Uuid,
    facility: &MappedFacility,
    ps_intake_run_id: &str,
    raw_ps_snapshot: &Value,
    manually_edited_fields: &[&str],
) -> Result<Uuid, sqlx::Error> {
    let (facility_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO clients.facilities
            (company_id, name, street_address, city, state, zip, phone, email,
             units_count, primary_storage_offering, previous_pms, access_control_system,
             go_live_date, dropbox_folder_url, subdomain, subdomain_exists_in_qms_raw,
             system_email, website_url, source, ps_intake_run_id, raw_ps_snapshot,
             manually_edited_fields, last_synced_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                 $17, $18, 'process_street', $19, $20, $21, now())
         RETURNING id",
    )
    .bind(company_id)
    .bind(facility.name.as_deref().unwrap_or("(unnamed facility)"))
    .bind(&facility.street_address)
    .bind(&facility.city)
    .bind(&facility.state)
    .bind(&facility.zip)
    .bind(&facility.phone)
    .bind(&facility.email)
    .bind(facility.units_count)
    .bind(&facility.primary_storage_offering)
    .bind(&facility.previous_pms)
    .bind(&facility.access_control_system)
    .bind(facility.go_live_date)
    .bind(&facility.dropbox_folder_url)
    .bind(&facility.subdomain)
    .bind(&facility.subdomain_exists_in_qms_raw)
    .bind(&facility.system_email)
    .bind(&facility.website_url)
    .bind(ps_intake_run_id)
    .bind(raw_ps_snapshot)
    .bind(manually_edited_fields)
    .fetch_one(&mut **tx)
    .await?;

    Ok(facility_id)
}

/// Inserts every Facility Policies row a `MappedIntakeRun` carries
/// (Fees/Taxes/Delinquency/Coverage/Commission/Specials), against an
/// already-created `facility_id`, plus `people` -- **not** derived from
/// `mapped` here. PS's own owner/DM/manager fields carry no real
/// facility-level attribution (the same raw text is copy-pasted onto
/// every sister facility's own run), so which facility a person
/// actually belongs to is a call the confirmation screen's own People
/// chips make, not something this function should silently re-derive.
/// Callers with no reviewed selection at all (`ingest_intake_run`,
/// Phase 1's still-unwired direct path) pass `mapped.people()` as their
/// own fallback. Split out of `ingest_intake_run` so the split-creation
/// flow can apply the same policy/people population to a facility that
/// didn't just create its own company.
pub async fn insert_facility_policies_and_people(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    mapped: &MappedIntakeRun,
    raw_ps_snapshot: &Value,
    people: &[PersonAssignment],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO clients.facility_policies (facility_id, raw_ps_snapshot) VALUES ($1, $2)",
    )
    .bind(facility_id)
    .bind(raw_ps_snapshot)
    .execute(&mut **tx)
    .await?;

    for fee in &mapped.fees {
        sqlx::query(
            "INSERT INTO clients.policy_fees (facility_policies_id, fee_type, label, raw_value)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(facility_id)
        .bind(fee.fee_type)
        .bind(&fee.label)
        .bind(&fee.raw_value)
        .execute(&mut **tx)
        .await?;
    }

    if let Some(taxes) = &mapped.taxes {
        sqlx::query(
            "INSERT INTO clients.policy_taxes
                (facility_policies_id, sales_tax_applies_raw, sales_tax_rate_raw,
                 rent_tax_applies_raw, rent_tax_rate_raw, rent_tax_applies_to_all_units_raw,
                 other_one_time_taxes_raw, other_recurring_taxes_raw)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(facility_id)
        .bind(&taxes.sales_tax_applies_raw)
        .bind(&taxes.sales_tax_rate_raw)
        .bind(&taxes.rent_tax_applies_raw)
        .bind(&taxes.rent_tax_rate_raw)
        .bind(&taxes.rent_tax_applies_to_all_units_raw)
        .bind(&taxes.other_one_time_taxes_raw)
        .bind(&taxes.other_recurring_taxes_raw)
        .execute(&mut **tx)
        .await?;
    }

    for step in &mapped.delinquency_steps {
        sqlx::query(
            "INSERT INTO clients.policy_delinquency_steps
                (facility_policies_id, step_order, step_type, raw_value)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(facility_id)
        .bind(step.step_order)
        .bind(step.step_type)
        .bind(&step.raw_value)
        .execute(&mut **tx)
        .await?;
    }

    for tier in &mapped.coverage_tiers {
        sqlx::query(
            "INSERT INTO clients.policy_coverage_tiers
                (facility_policies_id, tier_number, total_coverage_amount_raw, cost_to_tenant_raw)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(facility_id)
        .bind(tier.tier_number)
        .bind(&tier.total_coverage_amount_raw)
        .bind(&tier.cost_to_tenant_raw)
        .execute(&mut **tx)
        .await?;
    }

    if let Some(commission) = &mapped.commission {
        sqlx::query(
            "INSERT INTO clients.policy_commission
                (facility_policies_id, commission_type_raw, dollar_amount_raw, percent_amount_raw)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(facility_id)
        .bind(&commission.commission_type_raw)
        .bind(&commission.dollar_amount_raw)
        .bind(&commission.percent_amount_raw)
        .execute(&mut **tx)
        .await?;
    }

    if let Some(specials) = &mapped.specials_raw_text {
        sqlx::query(
            "INSERT INTO clients.policy_specials (facility_policies_id, raw_text) VALUES ($1, $2)",
        )
        .bind(facility_id)
        .bind(specials)
        .execute(&mut **tx)
        .await?;
    }

    for assignment in people {
        let person = ParsedPerson {
            full_name: assignment.full_name.clone(),
            email: assignment.email.clone(),
            phone: assignment.phone.clone(),
        };
        link_person_to_facility(tx, facility_id, &person, &assignment.role).await?;
    }

    Ok(())
}

/// Inserts a company, its own facility, and every Facility Policies row
/// a `MappedIntakeRun` carries, plus the owner/district-manager/manager
/// people it parsed. Returns `(company_id, facility_id)`. A thin
/// wrapper over `insert_company`/`insert_facility`/
/// `insert_facility_policies_and_people` above -- the "one run, one
/// company, one facility" shape (test-only now: used by this module's
/// live-DB test); the split-creation flow
/// (`clients::create`) calls the three building blocks directly instead,
/// since it needs a company created from a *different* run than some of
/// its facilities.
#[cfg(test)]
pub async fn ingest_intake_run(
    tx: &mut Transaction<'_, Postgres>,
    mapped: &MappedIntakeRun,
    ps_intake_run_id: &str,
    raw_ps_snapshot: &Value,
) -> Result<(Uuid, Uuid), sqlx::Error> {
    let legal_name = mapped
        .company
        .legal_name
        .as_deref()
        .unwrap_or("(unnamed company)");
    let company_id = insert_company(
        tx,
        legal_name,
        &mapped.company,
        ps_intake_run_id,
        raw_ps_snapshot,
        &[],
    )
    .await?;
    let facility_id = insert_facility(
        tx,
        company_id,
        &mapped.facility,
        ps_intake_run_id,
        raw_ps_snapshot,
        &[],
    )
    .await?;
    insert_facility_policies_and_people(tx, facility_id, mapped, raw_ps_snapshot, &mapped.people())
        .await?;
    Ok((company_id, facility_id))
}
