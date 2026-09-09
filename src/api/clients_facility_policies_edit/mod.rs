//! Manual editing for the split Fees/Taxes/Delinquency/Coverage/Specials
//! tabs -- the first editable data anywhere in this app. Each handler
//! replaces one category's data wholesale (simplest correct semantics
//! for a form save, matching how `api::clients_facility_people` already
//! treats "Add User" as an upsert rather than a patch) and then, via
//! `clients::policy_exemption::mark_exempt_if_qsx_and_was_empty`, flags
//! that category permanently exempt from any future policy-sync pass --
//! but only when it was genuinely empty before this write and the
//! facility is QSX-legacy. A category that already had real data (from
//! Process Street or a previous manual edit) never gets that exemption;
//! it stays subject to whatever conflict resolution a future
//! policy-sync extension of the "Re-sync" screen adds.
//!
//! No extra permission check beyond authentication -- RLS already gates
//! every one of these tables' INSERT/UPDATE/DELETE to
//! `onboarding_manager`/`department_manager` (see the schema migration's
//! own per-table policy loop), the same reasoning
//! `clients_facility_people`'s own module doc gives for skipping a
//! second, app-level check.
//!
//! 2026-09-09: split into one submodule per category -- each handler is
//! independent, and the five were previously stacked in a single ~1000
//! line file. Only the facility lookup and the standard error responses
//! below are actually shared.

use axum::{http::HeaderMap, response::Response};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

mod coverage;
mod delinquency;
mod fees;
mod specials;
mod taxes;

pub use coverage::update_coverage;
pub use delinquency::update_delinquency;
pub use fees::update_fees;
pub use specials::update_specials;
pub use taxes::update_taxes;

pub(super) fn request_context(headers: &HeaderMap) -> Option<&str> {
    headers.get(axum::http::header::USER_AGENT).and_then(|value| value.to_str().ok())
}

pub(super) fn not_found() -> Response {
    crate::api::not_found("not_found", "No such facility.".to_string())
}

pub(super) fn bad_request(message: String) -> Response {
    crate::api::bad_request("invalid_request", message)
}

pub(super) async fn ensure_facility_and_policies_row(
    tx: &mut Transaction<'_, Postgres>,
    company_id: Uuid,
    facility_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let exists: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM clients.facilities WHERE id = $1 AND company_id = $2")
            .bind(facility_id)
            .bind(company_id)
            .fetch_optional(&mut **tx)
            .await?;
    if exists.is_none() {
        return Ok(false);
    }

    // A facility that never had any Facility Policies data at all (a
    // manual facility, or one ingested before this row existed) has no
    // `facility_policies` row yet -- every category's child tables FK
    // reference it, so it must exist before any of them can.
    sqlx::query("INSERT INTO clients.facility_policies (facility_id) VALUES ($1) ON CONFLICT (facility_id) DO NOTHING")
        .bind(facility_id)
        .execute(&mut **tx)
        .await?;

    Ok(true)
}
