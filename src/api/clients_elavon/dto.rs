//! Response and request shapes for the Elavon tab.

use serde::Serialize;

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ElavonPartyInfo {
    pub party_role: String,
    pub display_name: Option<String>,
    pub title: Option<String>,
    pub ownership_percent: Option<f64>,
    pub email: Option<String>,
    pub phone: Option<String>,
    /// The real decrypted SSN -- masking (fully, with a Show/Hide reveal
    /// toggle) is deliberately a frontend concern (`PartyCard`), not done
    /// here, since Boris wants the real value revealable on demand.
    pub ssn: Option<String>,
    pub dob: Option<String>,
    pub home_address_line1: Option<String>,
    pub home_city: Option<String>,
    pub home_state_or_province: Option<String>,
    pub home_postal_code: Option<String>,
}

/// Facility-level financial data shown on the Elavon tab (2026-09-03) --
/// EIN and the bank routing/account numbers (decrypted from
/// `encrypted_secrets`, masked to their last 4 digits before leaving the
/// backend -- see `mask_bank_number`'s own doc comment), plus the
/// revenue/volume fields New Merchant Account's Facility Information
/// (Pre-App) step captures. `None` throughout when this facility has no
/// `encrypted_secrets` blob at all (a Merchant Account run ingested
/// before this field existed) or its decryption fails -- degrades this
/// one section rather than the whole tab, same pattern `ElavonPartyInfo`
/// already uses for a party's PII.
#[derive(Debug, Serialize, Default, ts_rs::TS)]
#[ts(export)]
pub struct ElavonFinancials {
    pub ein: Option<String>,
    pub bank_routing_number_masked: Option<String>,
    pub bank_account_number_masked: Option<String>,
    pub total_annual_business_revenue_raw: Option<String>,
    pub total_monthly_sales_raw: Option<String>,
    pub average_credit_card_payment_amount_raw: Option<String>,
    pub highest_credit_card_payment_amount_raw: Option<String>,
    pub high_cc_payment_times_per_year_raw: Option<String>,
    pub offers_ach_raw: Option<String>,
    pub annual_electronic_check_volume_raw: Option<String>,
    pub average_electronic_check_amount_raw: Option<String>,
    pub maximum_electronic_check_amount_raw: Option<String>,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ElavonCandidate {
    pub merchant_account_run_id: String,
    pub run_name: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// QuikStor's own fixed QMS web login username -- the same for every
/// facility. Confirmed live against Process Street (2026-09-09, the
/// Dubuqueland Upper Lot run the "Add Credentials to QMS" ticket linked
/// to): it's static text in that task's own template, not a per-run form
/// field the way `account_id`/`pin_password` are -- there is nothing to
/// fetch or resync for it.
pub(super) const QMS_WEB_USER_ID: &str = "QSSWEB";

/// The QMS Credentials half of the Elavon tab's credentials section
/// (2026-09-09) -- mirrors the "Add Credentials to QMS" checklist step's
/// own 3 lines (`account_id`/`pin_password` are real PS fields,
/// `user_id` is `QMS_WEB_USER_ID` above). `pin_password` is the real
/// decrypted value -- same "revealable on demand" convention
/// `ElavonPartyInfo.ssn` already uses, masking is a frontend concern.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ElavonQmsCredentials {
    pub account_id: Option<String>,
    pub user_id: &'static str,
    pub pin_password: Option<String>,
}

impl ElavonQmsCredentials {
    pub(super) fn empty() -> Self {
        Self {
            account_id: None,
            user_id: QMS_WEB_USER_ID,
            pin_password: None,
        }
    }
}

/// The Pin Pad Credentials half -- present only for a client that
/// actually got a pin pad (per the same PS ticket's own "If customer got
/// a Pin Pad" conditional); `None`/`None` throughout otherwise, same as
/// every other not-answered field in this tab.
#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ElavonPinpadCredentials {
    pub pinpad_user_id: Option<String>,
    pub qss_api_pin: Option<String>,
}

impl ElavonPinpadCredentials {
    pub(super) fn empty() -> Self {
        Self {
            pinpad_user_id: None,
            qss_api_pin: None,
        }
    }
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[serde(tag = "status", rename_all = "snake_case")]
#[ts(export)]
pub enum ElavonStatusResponse {
    Linked {
        rate_provided: Option<String>,
        application_status: Option<String>,
        credentials_added_to_qms: bool,
        ps_new_merchant_run_id: Option<String>,
        last_synced_at: Option<chrono::DateTime<chrono::Utc>>,
        parties: Vec<ElavonPartyInfo>,
        // Boxed -- clippy::large_enum_variant. `ElavonFinancials` is 9
        // Option<String> fields plus 3 more, which otherwise makes this
        // variant nearly 4.5x the size of `Unlinked`, forcing every
        // `ElavonStatusResponse` (including the far more common Unlinked
        // case) to be sized for the largest variant.
        financials: Box<ElavonFinancials>,
        qms_credentials: ElavonQmsCredentials,
        pinpad_credentials: ElavonPinpadCredentials,
    },
    Unlinked {
        /// Present when title correlation found exactly one candidate
        /// (see `Correlation::Unambiguous`). Absent -- not an error --
        /// when there's genuinely no candidate at all, or the match was
        /// ambiguous (`ambiguous_candidates` below is used instead in
        /// that case).
        candidate: Option<ElavonCandidate>,
        /// Populated instead of `candidate` when title correlation found
        /// more than one match (`Correlation::Ambiguous`) -- a real
        /// duplicate submission, confirmed against Carpentersville's own
        /// data (see `merchant_account_correlation`'s own module doc).
        /// Never auto-picked, but shown as real options rather than
        /// forcing pure manual entry -- same idea `clients_search`'s own
        /// "Potential Duplicates" rows use.
        ambiguous_candidates: Vec<ElavonCandidate>,
    },
}
