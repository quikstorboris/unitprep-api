//! Who is a client's Implementation Manager: the Process Street Intake
//! form's "Who is the Conductor" step, answered by an email address
//! (`Conductor_Email_Address:`), matched to the OO user with that email.
//!
//! A raw address is looked at in two places, in this order:
//!
//! 1. `clients.staff_identity_alias` -- the sanctioned redirect for an
//!    address that no longer maps to the right person (a former employee
//!    still named on old runs; see that table's migration), so the
//!    redirect is a data row, never a branch here;
//! 2. the user directory, by email (case-insensitive).
//!
//! No match leaves the manager unassigned -- never a guess. The company
//! page and the Clients filter only show an assigned user, and the
//! assignment can be made once the person has an OO account (the nightly
//! sync does not redo it, so a later account needs the one-line SQL in
//! the migration that introduced this, or the future reassign feature).

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::clients::fields::value_for;
use crate::process_street::FormField;

/// PS's answer to "Who is the Conductor": the conductor's email address.
const CONDUCTOR_EMAIL_KEY: &str = "Conductor_Email_Address:";

/// The OO user a raw email resolves to, or `None`.
pub async fn resolve_user_by_email(
    tx: &mut Transaction<'_, Postgres>,
    raw_email: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    let raw = raw_email.trim();
    if raw.is_empty() {
        return Ok(None);
    }

    let aliased: Option<Uuid> = sqlx::query_scalar(
        "SELECT resolved_user_id FROM clients.staff_identity_alias
          WHERE lower(raw_identifier) = lower($1)",
    )
    .bind(raw)
    .fetch_optional(&mut **tx)
    .await?;
    if aliased.is_some() {
        return Ok(aliased);
    }

    sqlx::query_scalar("SELECT id FROM auth.staff_directory() WHERE lower(email) = lower($1)")
        .bind(raw)
        .fetch_optional(&mut **tx)
        .await
}

/// Sets the company's Implementation Manager from the Intake run's
/// conductor email, when that resolves to an OO user and none is set.
/// Returns the assigned user, if any.
pub async fn assign_implementation_manager(
    tx: &mut Transaction<'_, Postgres>,
    company_id: Uuid,
    intake_fields: &[FormField],
) -> Result<Option<Uuid>, sqlx::Error> {
    let Some(email) = value_for(intake_fields, CONDUCTOR_EMAIL_KEY) else {
        return Ok(None);
    };
    let Some(user_id) = resolve_user_by_email(tx, &email).await? else {
        tracing::info!(%company_id, "the Intake conductor's email matches no OO user; Implementation Manager left unassigned");
        return Ok(None);
    };

    sqlx::query(
        "UPDATE clients.companies SET implementation_manager_user_id = $1
          WHERE id = $2 AND implementation_manager_user_id IS NULL",
    )
    .bind(user_id)
    .bind(company_id)
    .execute(&mut **tx)
    .await?;

    Ok(Some(user_id))
}
