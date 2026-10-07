//! Builders behind the Elavon tab: decrypting a stored Merchant Account row
//! into the financials, credentials and parties the tab shows.

use super::dto::{
    ElavonFinancials, ElavonPartyInfo, ElavonPinpadCredentials, ElavonQmsCredentials,
    QMS_WEB_USER_ID,
};
use crate::clients::merchant_account_mapping::{
    decrypt_elavon_credentials, decrypt_facility_secrets, decrypt_party_pii, mask_bank_number,
};
use uuid::Uuid;

#[derive(sqlx::FromRow)]
pub(super) struct FacilityIdentity {
    pub(super) ps_intake_run_id: Option<String>,
}

#[derive(sqlx::FromRow)]
pub(super) struct ExistingMerchantAccountRow {
    pub(super) rate_provided: Option<String>,
    pub(super) application_status: Option<String>,
    pub(super) credentials_added_to_qms: bool,
    pub(super) ps_new_merchant_run_id: Option<String>,
    pub(super) last_synced_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(super) encrypted_secrets: Option<Vec<u8>>,
    pub(super) total_annual_business_revenue_raw: Option<String>,
    pub(super) total_monthly_sales_raw: Option<String>,
    pub(super) average_credit_card_payment_amount_raw: Option<String>,
    pub(super) highest_credit_card_payment_amount_raw: Option<String>,
    pub(super) high_cc_payment_times_per_year_raw: Option<String>,
    pub(super) offers_ach_raw: Option<String>,
    pub(super) annual_electronic_check_volume_raw: Option<String>,
    pub(super) average_electronic_check_amount_raw: Option<String>,
    pub(super) maximum_electronic_check_amount_raw: Option<String>,
}

/// Decrypts `existing.encrypted_secrets` (when present) into the
/// financials shown on the Elavon tab -- EIN unmasked (not asked to be
/// masked, and less sensitive than a personal SSN/bank account), bank
/// routing/account numbers masked to their last 4 digits. A decrypt
/// failure degrades to `None` for just the EIN/bank fields (logged, not
/// surfaced to the caller) -- the revenue/volume fields below don't
/// depend on `encrypted_secrets` at all and are unaffected either way.
pub(super) fn build_financials(
    facility_id: Uuid,
    existing: &ExistingMerchantAccountRow,
) -> ElavonFinancials {
    let secrets = existing.encrypted_secrets.as_deref().and_then(|blob| {
        match decrypt_facility_secrets(facility_id, blob) {
            Ok(secrets) => Some(secrets),
            Err(err) => {
                tracing::error!(
                    error = %err,
                    facility_id = %facility_id,
                    "failed to decrypt facility secrets for the Elavon tab"
                );
                None
            }
        }
    });

    ElavonFinancials {
        ein: secrets.as_ref().and_then(|s| s.ein.clone()),
        bank_routing_number_masked: secrets
            .as_ref()
            .and_then(|s| s.bank_routing_number.as_deref())
            .map(mask_bank_number),
        bank_account_number_masked: secrets
            .as_ref()
            .and_then(|s| s.bank_account_number.as_deref())
            .map(mask_bank_number),
        total_annual_business_revenue_raw: existing.total_annual_business_revenue_raw.clone(),
        total_monthly_sales_raw: existing.total_monthly_sales_raw.clone(),
        average_credit_card_payment_amount_raw: existing
            .average_credit_card_payment_amount_raw
            .clone(),
        highest_credit_card_payment_amount_raw: existing
            .highest_credit_card_payment_amount_raw
            .clone(),
        high_cc_payment_times_per_year_raw: existing.high_cc_payment_times_per_year_raw.clone(),
        offers_ach_raw: existing.offers_ach_raw.clone(),
        annual_electronic_check_volume_raw: existing.annual_electronic_check_volume_raw.clone(),
        average_electronic_check_amount_raw: existing.average_electronic_check_amount_raw.clone(),
        maximum_electronic_check_amount_raw: existing.maximum_electronic_check_amount_raw.clone(),
    }
}

/// Decrypts `existing.encrypted_secrets` (when present) into the QMS
/// Credentials / Pin Pad Credentials sections -- same decrypt-failure
/// degradation as `build_financials` (logs, returns both halves empty
/// rather than failing the whole tab).
pub(super) fn build_credentials(
    facility_id: Uuid,
    existing: &ExistingMerchantAccountRow,
) -> (ElavonQmsCredentials, ElavonPinpadCredentials) {
    let credentials = existing.encrypted_secrets.as_deref().and_then(|blob| {
        match decrypt_elavon_credentials(facility_id, blob) {
            Ok(credentials) => Some(credentials),
            Err(err) => {
                tracing::error!(
                    error = %err,
                    facility_id = %facility_id,
                    "failed to decrypt facility secrets for the Elavon tab's credentials sections"
                );
                None
            }
        }
    });

    let Some(credentials) = credentials else {
        return (
            ElavonQmsCredentials::empty(),
            ElavonPinpadCredentials::empty(),
        );
    };

    (
        ElavonQmsCredentials {
            account_id: credentials.account_id,
            user_id: QMS_WEB_USER_ID,
            pin_password: credentials.qss_web_pin,
        },
        ElavonPinpadCredentials {
            pinpad_user_id: credentials.pinpad_user_id,
            qss_api_pin: credentials.qss_api_pin,
        },
    )
}

#[derive(sqlx::FromRow)]
pub(super) struct PartyRow {
    pub(super) party_role: String,
    pub(super) party_index: i32,
    pub(super) display_name: Option<String>,
    pub(super) title: Option<String>,
    pub(super) ownership_percent: Option<f64>,
    pub(super) email: Option<String>,
    pub(super) phone: Option<String>,
    pub(super) encrypted_pii: Option<Vec<u8>>,
}

pub(super) fn decrypt_parties(facility_id: Uuid, rows: Vec<PartyRow>) -> Vec<ElavonPartyInfo> {
    rows.into_iter()
        .map(|row| {
            let pii = row.encrypted_pii.as_deref().and_then(|blob| {
                match decrypt_party_pii(facility_id, &row.party_role, row.party_index, blob) {
                    Ok(pii) => Some(pii),
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            facility_id = %facility_id,
                            party_role = %row.party_role,
                            party_index = row.party_index,
                            "failed to decrypt a party's PII for the Elavon tab"
                        );
                        None
                    }
                }
            });

            ElavonPartyInfo {
                party_role: row.party_role,
                display_name: row.display_name,
                title: row.title,
                ownership_percent: row.ownership_percent,
                email: row.email,
                phone: row.phone,
                ssn: pii.as_ref().and_then(|p| p.ssn.clone()),
                dob: pii.as_ref().and_then(|p| p.dob.clone()),
                home_address_line1: pii.as_ref().and_then(|p| p.home_address_line1.clone()),
                home_city: pii.as_ref().and_then(|p| p.home_city.clone()),
                home_state_or_province: pii.as_ref().and_then(|p| p.home_state_or_province.clone()),
                home_postal_code: pii.as_ref().and_then(|p| p.home_postal_code.clone()),
            }
        })
        .collect()
}
