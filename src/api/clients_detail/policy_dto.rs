//! Response shapes for a facility's policies (fees, taxes, delinquency, coverage, specials).

use serde::Serialize;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct FeeRow {
    pub fee_type: String,
    pub label: Option<String>,
    pub raw_value: String,
}

/// Legacy free-text shape, read-only from here on -- superseded
/// 2026-09-08 by `TaxEntryRow`/`policy_tax_entries` below, but never
/// dropped: Highway 20 Self Storage has one real row here, and turning
/// its prose into the new structured fields needs a human, not a
/// migration. Still returned so that history isn't simply hidden.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TaxesRow {
    pub sales_tax_applies_raw: Option<String>,
    pub sales_tax_rate_raw: Option<String>,
    pub rent_tax_applies_raw: Option<String>,
    pub rent_tax_rate_raw: Option<String>,
    pub rent_tax_applies_to_all_units_raw: Option<String>,
    pub other_one_time_taxes_raw: Option<String>,
    pub other_recurring_taxes_raw: Option<String>,
}

/// Legacy free-text shape, read-only from here on -- superseded
/// 2026-09-08 by `DelinquencyEntryRow`/`policy_delinquency_entries`
/// below. Highway 20 Self Storage has 9 real rows here (one of which
/// names two separate fees in the same free-text value) -- kept
/// visible as history, not migrated automatically.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct DelinquencyStepRow {
    pub step_order: i32,
    pub step_type: String,
    pub raw_value: String,
}

/// `flat_amount`/`attribute_payable_percent` are NUMERIC columns, cast
/// to `float8` in every query that reads them (same convention
/// `clients_elavon`/`clients_detail`'s own `ownership_percent::float8`
/// already uses) -- avoids adding sqlx's `bigdecimal`/`rust_decimal`
/// feature just for two fields.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TaxEntryRow {
    pub id: i64,
    pub tax_type: String,
    pub tax_name: String,
    pub description: Option<String>,
    pub flat_amount: Option<f64>,
    pub attribute_payable_percent: Option<f64>,
    pub is_recurring: bool,
    pub sort_order: i32,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct DelinquencyEntryRow {
    pub id: i64,
    pub category: String,
    pub name: String,
    pub amount: f64,
    pub days_after: Option<i32>,
    pub trigger_type: String,
    pub trigger_category: Option<String>,
    pub sort_order: i32,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CoverageTierRow {
    pub tier_number: i32,
    pub total_coverage_amount_raw: Option<String>,
    pub cost_to_tenant_raw: Option<String>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CommissionRow {
    pub commission_type_raw: Option<String>,
    pub dollar_amount_raw: Option<String>,
    pub percent_amount_raw: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FacilityPoliciesResponse {
    pub fees: Vec<FeeRow>,
    /// Legacy free-text -- see `TaxesRow`'s own doc comment.
    pub taxes: Option<TaxesRow>,
    pub tax_entries: Vec<TaxEntryRow>,
    /// Legacy free-text -- see `DelinquencyStepRow`'s own doc comment.
    pub delinquency_steps: Vec<DelinquencyStepRow>,
    pub delinquency_entries: Vec<DelinquencyEntryRow>,
    pub coverage_tiers: Vec<CoverageTierRow>,
    pub commission: Option<CommissionRow>,
    pub specials_raw_text: Option<String>,
    /// Whether this facility's own `previous_pms` names QSX -- see
    /// `clients::policy_exemption`'s own module doc. Tells the frontend
    /// whether to offer manual entry at all for a category that's
    /// currently empty (only a QSX-legacy facility gets that permanent
    /// exemption once it's used).
    pub is_qsx_legacy: bool,
    pub fees_manually_exempt: bool,
    pub taxes_manually_exempt: bool,
    pub delinquency_manually_exempt: bool,
    pub coverage_manually_exempt: bool,
    pub specials_manually_exempt: bool,
}
