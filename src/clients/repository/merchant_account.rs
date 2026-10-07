//! Ingesting and re-syncing a facility's Merchant Account run and its parties.

use crate::clients::encryption::EncryptionError;
use crate::clients::merchant_account_mapping::{MappedMerchantAccount, MappedParty};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

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

/// Refreshes an already-linked facility's whole Merchant Account
/// picture in place -- same data `ingest_merchant_account_run` writes
/// for a brand-new link, but UPDATEs the existing
/// `facility_merchant_accounts` row instead of inserting a new one, and
/// replaces every party row (delete, then re-insert each fresh one)
/// rather than assuming none exist yet. Backs the Elavon tab's "Resync
/// Elavon Data" action (2026-09-09) -- the fix for `credentials_added_to_qms`
/// (and everything else on this tab) having no refresh path short of a
/// destructive unlink/relink once PS's own data changes after the
/// initial link (e.g. the "Add Credentials to QMS" checklist step gets
/// completed later). Unlike Intake's own resync, nothing here has a
/// manual-edit UI in OO to protect -- this data is pure read-through
/// from PS -- so a full silent overwrite is safe, no field-by-field
/// conflict review needed.
///
/// `ps_new_merchant_run_id` is not part of the UPDATE -- resync always
/// refreshes the same run a facility is already linked to (switching to
/// a *different* run is `unlink` then `link`, not this), it's only
/// needed here to re-stamp each freshly-inserted party row the same way
/// `insert_party` already does at link time.
pub async fn resync_merchant_account_run(
    tx: &mut Transaction<'_, Postgres>,
    facility_id: Uuid,
    mapped: &MappedMerchantAccount,
    ps_new_merchant_run_id: &str,
    credentials_added_to_qms: bool,
) -> Result<(), IngestMerchantAccountError> {
    let encrypted_secrets = mapped.encrypted_secrets(facility_id)?;

    sqlx::query(
        "UPDATE clients.facility_merchant_accounts
         SET rate_provided = $2, application_status = $3, credentials_added_to_qms = $4,
             raw_ps_snapshot = $5, encrypted_secrets = $6,
             total_annual_business_revenue_raw = $7, total_monthly_sales_raw = $8,
             average_credit_card_payment_amount_raw = $9, highest_credit_card_payment_amount_raw = $10,
             high_cc_payment_times_per_year_raw = $11, offers_ach_raw = $12,
             annual_electronic_check_volume_raw = $13, average_electronic_check_amount_raw = $14,
             maximum_electronic_check_amount_raw = $15, last_synced_at = now()
         WHERE facility_id = $1",
    )
    .bind(facility_id)
    .bind(&mapped.rate_provided)
    .bind(&mapped.application_status)
    .bind(credentials_added_to_qms)
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

    sqlx::query("DELETE FROM clients.facility_merchant_account_parties WHERE facility_id = $1")
        .bind(facility_id)
        .execute(&mut **tx)
        .await?;

    for party in &mapped.parties {
        insert_party(tx, facility_id, party, ps_new_merchant_run_id).await?;
    }

    Ok(())
}

pub(super) async fn insert_party(
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
