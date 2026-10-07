//! The read queries behind a facility's policies, one per policy table.

use super::policy_dto::{
    CommissionRow, CoverageTierRow, DelinquencyEntryRow, DelinquencyStepRow, FeeRow, TaxEntryRow,
    TaxesRow,
};
use crate::auth::begin_rls_transaction;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
pub(super) struct FacilityPolicyFlags {
    pub(super) fees_manually_exempt: bool,
    pub(super) taxes_manually_exempt: bool,
    pub(super) delinquency_manually_exempt: bool,
    pub(super) coverage_manually_exempt: bool,
    pub(super) specials_manually_exempt: bool,
}

pub(super) async fn fetch_facility_exists(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    company_id: Uuid,
    facility_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM clients.facilities WHERE id = $1 AND company_id = $2")
            .bind(facility_id)
            .bind(company_id)
            .fetch_optional(&mut *tx)
            .await?;
    tx.commit().await?;
    Ok(row.is_some())
}

pub(super) async fn fetch_policy_fees(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Vec<FeeRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let rows = sqlx::query_as(
        "SELECT fee_type, label, raw_value FROM clients.policy_fees \
         WHERE facility_policies_id = $1 ORDER BY id",
    )
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

pub(super) async fn fetch_tax_entries(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Vec<TaxEntryRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let rows = sqlx::query_as(
        "SELECT id, tax_type, tax_name, description, flat_amount::float8 AS flat_amount, \
         attribute_payable_percent::float8 AS attribute_payable_percent, is_recurring, sort_order \
         FROM clients.policy_tax_entries WHERE facility_policies_id = $1 ORDER BY sort_order",
    )
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

pub(super) async fn fetch_delinquency_entries(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Vec<DelinquencyEntryRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let rows = sqlx::query_as(
        "SELECT id, category, name, amount::float8 AS amount, days_after, trigger_type, trigger_category, sort_order \
         FROM clients.policy_delinquency_entries WHERE facility_policies_id = $1 ORDER BY sort_order",
    )
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

pub(super) async fn fetch_policy_taxes(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Option<TaxesRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let row = sqlx::query_as(
        "SELECT sales_tax_applies_raw, sales_tax_rate_raw, rent_tax_applies_raw, rent_tax_rate_raw, \
         rent_tax_applies_to_all_units_raw, other_one_time_taxes_raw, other_recurring_taxes_raw \
         FROM clients.policy_taxes WHERE facility_policies_id = $1",
    )
    .bind(facility_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

pub(super) async fn fetch_delinquency_steps(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Vec<DelinquencyStepRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let rows = sqlx::query_as(
        "SELECT step_order, step_type, raw_value FROM clients.policy_delinquency_steps \
         WHERE facility_policies_id = $1 ORDER BY step_order",
    )
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

pub(super) async fn fetch_coverage_tiers(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Vec<CoverageTierRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let rows = sqlx::query_as(
        "SELECT tier_number, total_coverage_amount_raw, cost_to_tenant_raw \
         FROM clients.policy_coverage_tiers WHERE facility_policies_id = $1 ORDER BY tier_number",
    )
    .bind(facility_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

pub(super) async fn fetch_commission(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Option<CommissionRow>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let row = sqlx::query_as(
        "SELECT commission_type_raw, dollar_amount_raw, percent_amount_raw \
         FROM clients.policy_commission WHERE facility_policies_id = $1",
    )
    .bind(facility_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

pub(super) async fn fetch_specials_raw_text(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;
    let row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT raw_text FROM clients.policy_specials WHERE facility_policies_id = $1",
    )
    .bind(facility_id)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row.and_then(|(text,)| text))
}

/// A facility with no `facility_policies` row at all yet (never
/// ingested, or a manual facility whose first category edit hasn't
/// happened) has every exempt flag false -- there's nothing to exempt
/// until a write handler creates that row.
pub(super) async fn fetch_policy_flags_and_qsx_status(
    db: &sqlx::PgPool,
    user_id: Uuid,
    role_keys: &[String],
    facility_id: Uuid,
) -> Result<(FacilityPolicyFlags, bool), sqlx::Error> {
    let mut tx = begin_rls_transaction(db, user_id, role_keys).await?;

    let flags: Option<FacilityPolicyFlags> = sqlx::query_as(
        "SELECT fees_manually_exempt, taxes_manually_exempt, delinquency_manually_exempt, \
         coverage_manually_exempt, specials_manually_exempt \
         FROM clients.facility_policies WHERE facility_id = $1",
    )
    .bind(facility_id)
    .fetch_optional(&mut *tx)
    .await?;

    let previous_pms: Option<(Option<String>,)> =
        sqlx::query_as("SELECT previous_pms FROM clients.facilities WHERE id = $1")
            .bind(facility_id)
            .fetch_optional(&mut *tx)
            .await?;

    tx.commit().await?;

    let flags = flags.unwrap_or(FacilityPolicyFlags {
        fees_manually_exempt: false,
        taxes_manually_exempt: false,
        delinquency_manually_exempt: false,
        coverage_manually_exempt: false,
        specials_manually_exempt: false,
    });

    let is_qsx = previous_pms
        .and_then(|(pms,)| pms)
        .is_some_and(|pms| crate::clients::policy_exemption::is_qsx_legacy(Some(&pms)));

    Ok((flags, is_qsx))
}
