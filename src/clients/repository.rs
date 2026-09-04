//! Writes already-mapped Process Street data into the `clients` schema.
//! Every write runs against a caller-supplied RLS transaction (see
//! `auth::authenticated_user::begin_rls_transaction`) -- the same
//! mechanism every other write in this app uses, so these rows are
//! subject to the real onboarding_manager/department_manager RLS
//! gating, never a privileged bypass. Callers commit or roll back the
//! transaction themselves.
//!
//! Plain dynamic `sqlx::query`/`query_as`, matching this codebase's
//! established convention -- there is no compile-time-checked
//! `query!`/`query_as!` usage anywhere in this crate (no `.sqlx` offline
//! cache exists), so this file doesn't introduce one either.
//!
//! Takes only already-mapped, already-encrypted data. This module never
//! imports `FacilitySecrets`/`PartyPii` (private to
//! `merchant_account_mapping`) and never sees a plaintext SSN -- it
//! only ever binds the `Vec<u8>` ciphertext `encrypted_pii`/
//! `encrypted_secrets` already produced by that module.

// Contract Order ingestion has no real caller yet (on hold per Boris,
// 2026-08-31 -- see the vault's own Implementation Plan) -- everything
// else here is called for real by `clients::create` and
// `api::clients_elavon`.
#![allow(dead_code)]

use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::clients::contract_order_mapping::MappedContractOrder;
use crate::clients::encryption::EncryptionError;
use crate::clients::intake_mapping::{MappedCompany, MappedFacility, MappedIntakeRun};
use crate::clients::merchant_account_mapping::{MappedMerchantAccount, MappedParty};
use crate::clients::people::{ParsedPerson, PersonAssignment};
use crate::process_street::Task;

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
    sqlx::query("INSERT INTO clients.facility_policies (facility_id, raw_ps_snapshot) VALUES ($1, $2)")
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
        sqlx::query("INSERT INTO clients.policy_specials (facility_policies_id, raw_text) VALUES ($1, $2)")
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
/// company, one facility" shape `clients::ingest::ingest_facility`
/// already uses and has proven live; the split-creation flow
/// (`clients::create`) calls the three building blocks directly instead,
/// since it needs a company created from a *different* run than some of
/// its facilities.
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
    let company_id =
        insert_company(tx, legal_name, &mapped.company, ps_intake_run_id, raw_ps_snapshot, &[]).await?;
    let facility_id =
        insert_facility(tx, company_id, &mapped.facility, ps_intake_run_id, raw_ps_snapshot, &[]).await?;
    insert_facility_policies_and_people(tx, facility_id, mapped, raw_ps_snapshot, &mapped.people()).await?;
    Ok((company_id, facility_id))
}

/// Finds an existing person by email (case-insensitive, via CITEXT) or
/// creates one, then links them to the facility with the given role --
/// idempotent via `ON CONFLICT DO NOTHING` on the (facility_id,
/// person_id, role) primary key, since the same person/role pair can
/// legitimately be re-ingested. Matching by name+phone alone (no email)
/// is deliberately NOT attempted here -- that's the same
/// manual-review-worthy fuzzy-match problem [[Dedup Tool Index|the
/// dedup tool]] exists to solve, not something to guess at inline.
async fn link_person_to_facility(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    person: &ParsedPerson,
    role: &str,
) -> Result<(), sqlx::Error> {
    let existing: Option<(Uuid,)> = match &person.email {
        Some(email) => sqlx::query_as("SELECT id FROM clients.people WHERE email = $1")
            .bind(email)
            .fetch_optional(&mut **tx)
            .await?,
        None => None,
    };

    let person_id = match existing {
        Some((id,)) => id,
        None => {
            let (id,): (Uuid,) = sqlx::query_as(
                "INSERT INTO clients.people (full_name, email, phone) VALUES ($1, $2, $3) RETURNING id",
            )
            .bind(&person.full_name)
            .bind(&person.email)
            .bind(&person.phone)
            .fetch_one(&mut **tx)
            .await?;
            id
        }
    };

    sqlx::query(
        "INSERT INTO clients.facility_people (facility_id, person_id, role)
         VALUES ($1, $2, $3)
         ON CONFLICT (facility_id, person_id, role) DO NOTHING",
    )
    .bind(facility_id)
    .bind(person_id)
    .bind(role)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Same find-by-email-or-create-then-link shape as `link_person_to_facility`
/// above, but for `api::clients_facility_people`'s "Add User" chips rather
/// than one-time ingest: when a matching `clients.people` row already
/// exists, its `full_name`/`phone` are overwritten with the caller's
/// values instead of left alone. `link_person_to_facility` deliberately
/// never does this (ingest only ever runs once per facility, so there's
/// nothing stale yet to correct at that point) -- this function exists
/// specifically for the case that same restraint can't cover: a person
/// already linked from an old ingest, whose stored name/phone drifted
/// from what `clients.ps_person_index` -- refreshed nightly, independent
/// of ingest -- currently says, e.g. Sand-Sto's own "Irene Chen -
/// (301) 787-9221" (a pre-fix dash-format parse glued onto her name;
/// confirmed live 2026-09-04, Boris's own call: the tab should self-heal
/// this on next use rather than needing a one-off backfill). Email is the
/// only identity key, same reasoning as `link_person_to_facility`'s own
/// doc comment -- name+phone fuzzy matching is dedup's problem, not this
/// function's.
pub async fn upsert_person_and_link_to_facility(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    assignment: &PersonAssignment,
) -> Result<(), sqlx::Error> {
    let existing: Option<(Uuid,)> = match &assignment.email {
        Some(email) => sqlx::query_as("SELECT id FROM clients.people WHERE email = $1")
            .bind(email)
            .fetch_optional(&mut **tx)
            .await?,
        None => None,
    };

    let person_id = match existing {
        Some((id,)) => {
            sqlx::query(
                "UPDATE clients.people SET full_name = $1, phone = $2, updated_at = now() WHERE id = $3",
            )
            .bind(&assignment.full_name)
            .bind(&assignment.phone)
            .bind(id)
            .execute(&mut **tx)
            .await?;
            id
        }
        None => {
            let (id,): (Uuid,) = sqlx::query_as(
                "INSERT INTO clients.people (full_name, email, phone) VALUES ($1, $2, $3) RETURNING id",
            )
            .bind(&assignment.full_name)
            .bind(&assignment.email)
            .bind(&assignment.phone)
            .fetch_one(&mut **tx)
            .await?;
            id
        }
    };

    sqlx::query(
        "INSERT INTO clients.facility_people (facility_id, person_id, role)
         VALUES ($1, $2, $3)
         ON CONFLICT (facility_id, person_id, role) DO NOTHING",
    )
    .bind(facility_id)
    .bind(person_id)
    .bind(&assignment.role)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Either half of what can go wrong writing a Merchant Account run: the
/// database, or `CLIENT_PII_ENCRYPTION_KEY` not being configured
/// (`clients::encryption`'s own concern -- see that module's own doc).
/// Was two separate `.expect()` panics until 2026-09-03, when the new
/// Elavon-tab "link" action became the first caller to actually hit the
/// missing-key case live (`clients::create`'s own callers had never
/// exercised it) -- a config problem should surface as a normal error
/// response, not a request-handler panic.
#[derive(Debug, thiserror::Error)]
pub enum IngestMerchantAccountError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("encryption error: {0}")]
    Encryption(#[from] EncryptionError),
}

/// Inserts the Elavon tab's facility-level data (rate/status/encrypted
/// secrets) and every party row (signer, owners, intermediary
/// businesses) for an already-existing facility.
///
/// `credentials_added_to_qms` is not part of `mapped` -- it's not a
/// form field at all, it's whether PS's own "Add Credentials to QMS"
/// checklist task is completed (see
/// `merchant_account_mapping::credentials_added_to_qms_from_tasks`,
/// which every real caller derives this from off a `get_run_tasks`
/// call the caller already needed to make for `ps_task_status` anyway).
pub async fn ingest_merchant_account_run(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    mapped: &MappedMerchantAccount,
    ps_new_merchant_run_id: &str,
    credentials_added_to_qms: bool,
) -> Result<(), IngestMerchantAccountError> {
    let encrypted_secrets = mapped.encrypted_secrets(facility_id)?;

    sqlx::query(
        "INSERT INTO clients.facility_merchant_accounts
            (facility_id, rate_provided, application_status, credentials_added_to_qms, source,
             ps_new_merchant_run_id, raw_ps_snapshot, encrypted_secrets,
             total_annual_business_revenue_raw, total_monthly_sales_raw,
             average_credit_card_payment_amount_raw, highest_credit_card_payment_amount_raw,
             high_cc_payment_times_per_year_raw, offers_ach_raw,
             annual_electronic_check_volume_raw, average_electronic_check_amount_raw,
             maximum_electronic_check_amount_raw, last_synced_at)
         VALUES ($1, $2, $3, $4, 'process_street', $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, now())",
    )
    .bind(facility_id)
    .bind(&mapped.rate_provided)
    .bind(&mapped.application_status)
    .bind(credentials_added_to_qms)
    .bind(ps_new_merchant_run_id)
    .bind(&mapped.sanitized_snapshot)
    .bind(encrypted_secrets)
    .bind(&mapped.total_annual_business_revenue_raw)
    .bind(&mapped.total_monthly_sales_raw)
    .bind(&mapped.average_credit_card_payment_amount_raw)
    .bind(&mapped.highest_credit_card_payment_amount_raw)
    .bind(&mapped.high_cc_payment_times_per_year_raw)
    .bind(&mapped.offers_ach_raw)
    .bind(&mapped.annual_electronic_check_volume_raw)
    .bind(&mapped.average_electronic_check_amount_raw)
    .bind(&mapped.maximum_electronic_check_amount_raw)
    .execute(&mut **tx)
    .await?;

    for party in &mapped.parties {
        insert_party(tx, facility_id, party, ps_new_merchant_run_id).await?;
    }

    Ok(())
}

async fn insert_party(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    party: &MappedParty,
    ps_new_merchant_run_id: &str,
) -> Result<(), IngestMerchantAccountError> {
    let encrypted_pii = party.encrypted_pii(facility_id)?;

    sqlx::query(
        "INSERT INTO clients.facility_merchant_account_parties
            (facility_id, party_role, party_index, display_name, title, ownership_percent,
             email, phone, country_of_citizenship, country, encrypted_pii, source,
             ps_new_merchant_run_id, last_synced_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'process_street', $12, now())",
    )
    .bind(facility_id)
    .bind(party.party_role)
    .bind(party.party_index)
    .bind(&party.display_name)
    .bind(&party.title)
    .bind(party.ownership_percent)
    .bind(&party.email)
    .bind(&party.phone)
    .bind(&party.country_of_citizenship)
    .bind(&party.country)
    .bind(encrypted_pii)
    .bind(ps_new_merchant_run_id)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Inserts a Contract Order run's data for an already-existing
/// facility. Nothing here is encrypted -- this workflow has no
/// sensitive fields, unlike New Merchant Account.
pub async fn ingest_contract_order_run(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    mapped: &MappedContractOrder,
    ps_contract_order_run_id: &str,
    raw_ps_snapshot: &Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO clients.facility_contract_orders
            (facility_id, migrating_from_system, source, ps_contract_order_run_id,
             raw_ps_snapshot, last_synced_at)
         VALUES ($1, $2, 'process_street', $3, $4, now())",
    )
    .bind(facility_id)
    .bind(&mapped.migrating_from_system)
    .bind(ps_contract_order_run_id)
    .bind(raw_ps_snapshot)
    .execute(&mut **tx)
    .await?;

    Ok(())
}

/// Upserts step-completion status for one workflow's tasks against an
/// already-existing facility. `workflow` is `intake` | `merchant_account`
/// | `contract_order`, matching `ps_task_status.workflow`'s CHECK
/// constraint.
pub async fn upsert_task_status(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    workflow: &str,
    tasks: &[Task],
) -> Result<(), sqlx::Error> {
    for task in tasks {
        sqlx::query(
            "INSERT INTO clients.ps_task_status
                (facility_id, workflow, ps_task_id, task_name, status, last_synced_at)
             VALUES ($1, $2, $3, $4, $5, now())
             ON CONFLICT (facility_id, workflow, ps_task_id)
             DO UPDATE SET task_name = EXCLUDED.task_name,
                           status = EXCLUDED.status,
                           last_synced_at = now()",
        )
        .bind(facility_id)
        .bind(workflow)
        .bind(&task.id)
        .bind(&task.name)
        .bind(&task.status)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod integration_tests {
    use serde_json::Value;
    use serial_test::serial;
    use uuid::Uuid;

    use crate::clients::contract_order_mapping::map_contract_order_fields;
    use crate::clients::intake_mapping::map_intake_fields;
    use crate::clients::merchant_account_mapping::map_merchant_account_fields;
    use crate::process_street::{FormField, Task};

    use super::*;

    // Same fixtures the unit tests in intake_mapping/merchant_account_mapping
    // use -- see those modules' own doc comments on why each is safe to
    // commit (Intake has no sensitive data at all; the Merchant Account
    // fixture has every sensitive value replaced with an obvious fake
    // before it was ever written to disk).
    const HIGHWAY20_INTAKE_FIELDS: &str =
        include_str!("testdata/highway20_intake_fields.json");
    const HIGHWAY20_INTAKE_TASKS: &str = include_str!("testdata/highway20_intake_tasks.json");
    const HIGHWAY20_NMA_FIELDS_SANITIZED: &str =
        include_str!("testdata/highway20_merchant_account_fields_sanitized.json");
    // A real Contract Order run for a different real client (Tri County
    // Mini Storage). Highway 20 does actually have its own real Contract
    // Order run too (discovered 2026-08-31 while testing clients::search
    // -- an earlier belief that it didn't was itself a casualty of the
    // status=Active-only bug this same session found and fixed), but
    // that data stays untouched here per Boris's explicit hold on
    // further Contract Order work. Tri County's run is grafted onto
    // Highway 20's facility_id purely to prove
    // ingest_contract_order_run's SQL is valid against the real,
    // migrated schema, the same reasoning
    // auth::authenticated_user's own `query_sessions_own_sql_is_valid_
    // against_the_real_schema` test uses.
    const TRI_COUNTY_CONTRACT_ORDER_FIELDS: &str =
        include_str!("testdata/tri_county_contract_order_fields.json");

    fn set_test_key() {
        std::env::set_var(
            "CLIENT_PII_ENCRYPTION_KEY",
            "2222222222222222222222222222222222222222222222222222222222222222",
        );
    }
    fn clear_test_key() {
        std::env::remove_var("CLIENT_PII_ENCRYPTION_KEY");
    }

    /// Full Phase 1 pipeline, proven end to end against the real,
    /// migrated `clients` schema on the real Neon dev branch -- not a
    /// mock, not an in-memory pool. Ingests the real (Intake) /
    /// sanitized-but-realistic (Merchant Account) Prairie Enterprises
    /// Highway 20 fixtures through the actual mapping + repository +
    /// RLS-transaction path a real request would use, then verifies
    /// the rows really landed correctly -- including decrypting the
    /// encrypted PII/secrets blobs back out of Postgres and confirming
    /// they match what was ingested -- before rolling back so nothing
    /// persists.
    ///
    /// Needs a real, reachable Postgres with every migration applied
    /// (`DATABASE_URL` from `.env.local`) -- `#[ignore]`d so the fast
    /// offline suite this crate otherwise is stays fast and offline.
    /// Run explicitly with `cargo test -- --ignored highway20_golden_fixture`.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
    #[serial(client_pii_encryption_key_env)]
    async fn highway20_golden_fixture_ingests_and_round_trips_through_real_postgres() {
        let _ = dotenvy::from_filename(".env.local");
        set_test_key();

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let user_id = Uuid::new_v4();
        let mut tx = crate::auth::begin_rls_transaction(
            &db,
            user_id,
            &["onboarding_manager".to_string()],
        )
        .await
        .expect("beginning an RLS transaction must succeed");

        let intake_fields: Vec<FormField> =
            serde_json::from_str(HIGHWAY20_INTAKE_FIELDS).expect("intake fixture must parse");
        let intake_tasks: Vec<Task> =
            serde_json::from_value(serde_json::from_str::<Value>(HIGHWAY20_INTAKE_TASKS).unwrap()["tasks"].take())
                .expect("intake tasks fixture must parse");
        let nma_fields: Vec<FormField> =
            serde_json::from_str(HIGHWAY20_NMA_FIELDS_SANITIZED).expect("NMA fixture must parse");

        let mapped_intake = map_intake_fields(&intake_fields);
        let mapped_nma = map_merchant_account_fields(&nma_fields);

        let (company_id, facility_id) = ingest_intake_run(
            &mut tx,
            &mapped_intake,
            "iy22NyiqGjwAAytKp0NErQ",
            &Value::Null,
        )
        .await
        .expect("ingesting the real Intake run must succeed");

        ingest_merchant_account_run(&mut tx, facility_id, &mapped_nma, "n1JtiN4m3mP-I0j8BChG4A", true)
            .await
            .expect("ingesting the sanitized Merchant Account run must succeed");

        upsert_task_status(&mut tx, facility_id, "intake", &intake_tasks)
            .await
            .expect("upserting task status must succeed");

        // --- Verify plain data landed correctly ---
        let (legal_name,): (String,) =
            sqlx::query_as("SELECT legal_name FROM clients.companies WHERE id = $1")
                .bind(company_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(legal_name, "Prairie Enterprises LLC");

        let (facility_name, units_count): (String, Option<i32>) = sqlx::query_as(
            "SELECT name, units_count FROM clients.facilities WHERE id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(facility_name, "Highway 20 Self Storage");
        assert_eq!(units_count, Some(788));

        let (company_subdomain,): (Option<String>,) =
            sqlx::query_as("SELECT subdomain FROM clients.companies WHERE id = $1")
                .bind(company_id)
                .fetch_one(&mut *tx)
                .await
                .unwrap();
        assert_eq!(company_subdomain.as_deref(), Some("prairie-enterprises.qms-email.com"));

        let (facility_subdomain, subdomain_exists_raw, system_email): (
            Option<String>,
            Option<String>,
            Option<String>,
        ) = sqlx::query_as(
            "SELECT subdomain, subdomain_exists_in_qms_raw, system_email
               FROM clients.facilities WHERE id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(facility_subdomain.as_deref(), Some("tenant.highway20selfstorage.com"));
        assert_eq!(subdomain_exists_raw.as_deref(), Some("No"));
        assert_eq!(system_email.as_deref(), Some("info@tenant.highway20selfstorage.com"));

        let (fee_count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM clients.policy_fees WHERE facility_policies_id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert!(fee_count >= 5, "named fees plus the Any Other Fees blob must all be present");

        let (tier_count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM clients.policy_coverage_tiers WHERE facility_policies_id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(tier_count, 5);

        let (task_count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM clients.ps_task_status WHERE facility_id = $1 AND workflow = 'intake'",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(task_count as usize, intake_tasks.len());

        // --- Verify the encrypted columns really round-trip through real Postgres ---
        let (encrypted_secrets,): (Option<Vec<u8>>,) = sqlx::query_as(
            "SELECT encrypted_secrets FROM clients.facility_merchant_accounts WHERE facility_id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let decrypted_secrets =
            crate::clients::encryption::decrypt(facility_id.as_bytes(), &encrypted_secrets.unwrap())
                .expect("facility secrets stored in real Postgres must decrypt");
        let secrets_json: Value = serde_json::from_slice(&decrypted_secrets).unwrap();
        assert_eq!(secrets_json["ein"], "111111111"); // the fixture's fake EIN

        let (owner1_encrypted_pii,): (Option<Vec<u8>>,) = sqlx::query_as(
            "SELECT encrypted_pii FROM clients.facility_merchant_account_parties
             WHERE facility_id = $1 AND party_role = 'owner' AND party_index = 1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let owner1_aad = format!("{facility_id}:owner:1");
        let decrypted_pii =
            crate::clients::encryption::decrypt(owner1_aad.as_bytes(), &owner1_encrypted_pii.unwrap())
                .expect("owner 1's PII stored in real Postgres must decrypt under its own AAD");
        let pii_json: Value = serde_json::from_slice(&decrypted_pii).unwrap();
        assert_eq!(pii_json["ssn"], "000000000"); // the fixture's fake SSN

        // ownership_percent is NUMERIC in Postgres -- sqlx has no
        // built-in decode from NUMERIC into plain f64, so every real
        // reader (api::clients_detail, api::clients_elavon) casts to
        // float8 in SQL. Regression coverage for the 2026-09-03 bug
        // where this decode error only ever surfaced once a facility
        // had a real party row for the first time (Boris's own live
        // Elavon-tab link, not caught by this test until now since it
        // never previously selected this column back out at all).
        let (owner1_ownership_percent,): (Option<f64>,) = sqlx::query_as(
            "SELECT ownership_percent::float8 FROM clients.facility_merchant_account_parties
             WHERE facility_id = $1 AND party_role = 'owner' AND party_index = 1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .expect("selecting ownership_percent::float8 back out of real Postgres must not error");
        assert_eq!(owner1_ownership_percent, Some(30.0));

        // credentials_added_to_qms passed in as `true` above must
        // actually land in the row, not silently default to the
        // schema's own `false` -- regression coverage for the
        // 2026-09-03 bug where this column was never in the INSERT's
        // column list at all.
        let (credentials_added_to_qms,): (bool,) = sqlx::query_as(
            "SELECT credentials_added_to_qms FROM clients.facility_merchant_accounts WHERE facility_id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert!(credentials_added_to_qms);

        // Revenue/volume fields (2026-09-03) -- regression coverage for
        // the bug where these were already in raw_ps_snapshot (never
        // sensitive, so never denylisted) but never had a named column
        // to land in, so the Elavon tab never showed them.
        let (revenue, ach_volume): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT total_annual_business_revenue_raw, annual_electronic_check_volume_raw
               FROM clients.facility_merchant_accounts WHERE facility_id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(revenue.as_deref(), Some("840000"));
        assert_eq!(ach_volume.as_deref(), Some("20000"));

        // --- Verify raw_ps_snapshot on the Merchant Account row never carries a sensitive key ---
        let (raw_snapshot,): (Value,) = sqlx::query_as(
            "SELECT raw_ps_snapshot FROM clients.facility_merchant_accounts WHERE facility_id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert!(!raw_snapshot.to_string().contains("111111111")); // the fake EIN must not leak into the plaintext snapshot

        // --- Verify ingest_contract_order_run's SQL is valid against the real schema ---
        let tri_county_fields: Vec<FormField> = serde_json::from_str(TRI_COUNTY_CONTRACT_ORDER_FIELDS)
            .expect("Tri County contract order fixture must parse");
        let mapped_contract_order = map_contract_order_fields(&tri_county_fields);
        let contract_order_snapshot: Value =
            serde_json::to_value(&tri_county_fields).unwrap_or(Value::Null);

        ingest_contract_order_run(
            &mut tx,
            facility_id,
            &mapped_contract_order,
            "iz7Jz_awRApa68WuMjtKHw",
            &contract_order_snapshot,
        )
        .await
        .expect("ingesting a real Contract Order run must succeed");

        let (stored_run_id,): (String,) = sqlx::query_as(
            "SELECT ps_contract_order_run_id FROM clients.facility_contract_orders WHERE facility_id = $1",
        )
        .bind(facility_id)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        assert_eq!(stored_run_id, "iz7Jz_awRApa68WuMjtKHw");

        tx.rollback().await.expect("rollback must succeed -- this test writes no real data");
        clear_test_key();
    }

    /// Proves `upsert_person_and_link_to_facility`'s actual point against
    /// real Postgres: a second call for the same email overwrites the
    /// stored name/phone rather than leaving them alone or creating a
    /// duplicate row -- the exact self-heal `api::clients_facility_people`
    /// relies on for a facility like Sand-Sto, whose saved roster still
    /// carries a pre-parser-fix garbled name. Needs a real, reachable
    /// Postgres with every migration applied, same as the golden fixture
    /// test above -- `#[ignore]`d for the same reason.
    #[tokio::test]
    #[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
    async fn upsert_person_and_link_to_facility_overwrites_a_stale_name_and_phone_on_second_call() {
        let _ = dotenvy::from_filename(".env.local");

        let db = crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");
        let user_id = Uuid::new_v4();
        let mut tx = crate::auth::begin_rls_transaction(&db, user_id, &["onboarding_manager".to_string()])
            .await
            .expect("beginning an RLS transaction must succeed");

        let (company_id,): (Uuid,) =
            sqlx::query_as("INSERT INTO clients.companies (legal_name, source) VALUES ($1, 'manual') RETURNING id")
                .bind("Test Upsert Co")
                .fetch_one(&mut *tx)
                .await
                .unwrap();

        let (facility_id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO clients.facilities (company_id, name, source) VALUES ($1, $2, 'manual') RETURNING id",
        )
        .bind(company_id)
        .bind("Test Upsert Facility")
        .fetch_one(&mut *tx)
        .await
        .unwrap();

        // First call: a garbled name, same shape the old dash-format
        // parser bug actually produced for Sand-Sto's own Irene Chen.
        upsert_person_and_link_to_facility(
            &mut tx,
            facility_id,
            &PersonAssignment {
                full_name: "Irene Chen - (301) 787-9221".to_string(),
                email: Some("irene@example.com".to_string()),
                phone: None,
                role: "owner".to_string(),
            },
        )
        .await
        .expect("first upsert must succeed");

        // Second call: the fresh, correctly-parsed values for the same
        // email -- what a chip click off `ps_person_index` sends today.
        upsert_person_and_link_to_facility(
            &mut tx,
            facility_id,
            &PersonAssignment {
                full_name: "Irene Chen".to_string(),
                email: Some("irene@example.com".to_string()),
                phone: Some("(301) 787-9221".to_string()),
                role: "owner".to_string(),
            },
        )
        .await
        .expect("second upsert must succeed");

        let people: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT p.full_name, p.email::text, p.phone
               FROM clients.facility_people fp
               JOIN clients.people p ON p.id = fp.person_id
              WHERE fp.facility_id = $1",
        )
        .bind(facility_id)
        .fetch_all(&mut *tx)
        .await
        .unwrap();

        assert_eq!(people.len(), 1, "the same email must never produce a second person or a second link");
        assert_eq!(people[0].0, "Irene Chen", "the stale, garbled name must be overwritten, not left alone");
        assert_eq!(people[0].2.as_deref(), Some("(301) 787-9221"));

        tx.rollback().await.expect("rollback must succeed -- this test writes no real data");
    }
}
