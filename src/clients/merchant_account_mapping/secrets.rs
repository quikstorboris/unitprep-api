//! The encrypted halves of a Merchant Account: sealing a facility's secrets and a party's PII, decrypting them again, and masking bank numbers. NOTE: facility secrets and Elavon credentials share the same AAD (the facility id) and key and are told apart only by plaintext shape -- do not change the AAD, it would break stored data.

use crate::clients::encryption::{self, EncryptionError};
use crate::clients::fields::value_for;
use crate::process_street::FormField;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Facility-level secrets, encrypted as one JSON bundle bound to the
/// facility. Never `pub` outside this module.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(super) struct FacilitySecrets {
    pub(super) ein: Option<String>,
    pub(super) bank_routing_number: Option<String>,
    pub(super) bank_account_number: Option<String>,
    pub(super) quikstor_password: Option<String>,
    pub(super) qss_web_pin: Option<String>,
    pub(super) pinpad_user_id: Option<String>,
    pub(super) qss_api_pin: Option<String>,
    pub(super) mid: Option<String>,
    pub(super) account_id: Option<String>,
}

impl FacilitySecrets {
    pub(super) fn is_empty(&self) -> bool {
        self.ein.is_none()
            && self.bank_routing_number.is_none()
            && self.bank_account_number.is_none()
            && self.quikstor_password.is_none()
            && self.qss_web_pin.is_none()
            && self.pinpad_user_id.is_none()
            && self.qss_api_pin.is_none()
            && self.mid.is_none()
            && self.account_id.is_none()
    }

    pub(super) fn from_fields(fields: &[FormField]) -> Self {
        Self {
            ein: value_for(fields, "EIN"),
            bank_routing_number: value_for(fields, "Bank_Routing_Number"),
            bank_account_number: value_for(fields, "Bank_Account_Number"),
            quikstor_password: value_for(fields, "QUIKSTOR_Password"),
            qss_web_pin: value_for(fields, "QSS_WEB_PIN"),
            pinpad_user_id: value_for(fields, "Pinpad_User_ID"),
            qss_api_pin: value_for(fields, "QSS_API_Pin"),
            mid: value_for(fields, "MID"),
            account_id: value_for(fields, "ACCOUNT_ID"),
        }
    }
}

/// One party's sensitive PII, encrypted as one JSON bundle bound to
/// that specific party (not just the facility) -- see
/// `clients::encryption`'s module doc on why the AAD goes this granular.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(super) struct PartyPii {
    pub(super) ssn: Option<String>,
    pub(super) dob: Option<String>,
    pub(super) home_address_line1: Option<String>,
    pub(super) home_city: Option<String>,
    pub(super) home_state_or_province: Option<String>,
    pub(super) home_postal_code: Option<String>,
}

impl PartyPii {
    pub(super) fn is_empty(&self) -> bool {
        self.ssn.is_none()
            && self.dob.is_none()
            && self.home_address_line1.is_none()
            && self.home_city.is_none()
            && self.home_state_or_province.is_none()
            && self.home_postal_code.is_none()
    }

    pub(super) fn from_fields(fields: &[FormField], prefix: &str) -> Self {
        Self {
            ssn: value_for(fields, &format!("{prefix}_-_SSN")),
            dob: value_for(fields, &format!("{prefix}_-_DOB")),
            home_address_line1: value_for(fields, &format!("{prefix}_-_HOME_Address")),
            home_city: value_for(fields, &format!("{prefix}_-_City")),
            home_state_or_province: value_for(fields, &format!("{prefix}_-_State_or_Province")),
            home_postal_code: value_for(fields, &format!("{prefix}_-_Postal_Code")),
        }
    }
}
/// Decrypted view of a party's PII, for the Company page's "Owner(s)
/// Information" section (Phase 4) -- the read counterpart to
/// `MappedParty::encrypted_pii`. A public, standalone mirror of the
/// private write-time `PartyPii` shape (same JSON field names, since
/// they're the same bytes) rather than a `pub use` of it, so the
/// write-time struct's own serde attributes can keep evolving
/// independently of what this read path promises callers.
#[derive(Debug, Clone, Deserialize)]
pub struct DecryptedPartyPii {
    pub ssn: Option<String>,
    pub dob: Option<String>,
    pub home_address_line1: Option<String>,
    pub home_city: Option<String>,
    pub home_state_or_province: Option<String>,
    pub home_postal_code: Option<String>,
}

/// Decrypts one party's `encrypted_pii` blob -- `aad` must be built the
/// exact same way `MappedParty::encrypted_pii` built it at write time
/// (`"{facility_id}:{party_role}:{party_index}"`), or decryption fails
/// by design (see that method's own doc comment on why the AAD is bound
/// this granularly).
pub fn decrypt_party_pii(
    facility_id: Uuid,
    party_role: &str,
    party_index: i32,
    blob: &[u8],
) -> Result<DecryptedPartyPii, EncryptionError> {
    let aad = format!("{facility_id}:{party_role}:{party_index}");
    let plaintext = encryption::decrypt(aad.as_bytes(), blob)?;
    serde_json::from_slice(&plaintext)
        .map_err(|_| EncryptionError::Undecryptable("malformed PartyPii plaintext"))
}

/// Decrypted view of the 3 `FacilitySecrets` fields the Elavon tab's
/// Financials section is allowed to surface (2026-09-03) -- EIN and the
/// bank routing/account numbers, masked by `mask_bank_number` before
/// ever reaching a caller outside this module. Deliberately NOT a `pub
/// use` of the private write-time `FacilitySecrets` (same reasoning as
/// `DecryptedPartyPii`'s own doc comment): this type omits the
/// QMS/processor system credentials (`quikstor_password`, `qss_web_pin`,
/// `pinpad_user_id`, `qss_api_pin`, `mid`, `account_id`) entirely --
/// serde silently drops unknown JSON keys on deserialize (no
/// `deny_unknown_fields` on either struct), so those fields never get
/// assigned to anything callable code could accidentally serialize back
/// out. See `DecryptedElavonCredentials` below for the similarly-scoped
/// type that now surfaces 4 of those 6 (2026-09-09, the QMS Credentials
/// / Pin Pad Credentials sections) -- `quikstor_password` and `mid`
/// still have no display path anywhere.
#[derive(Debug, Clone, Deserialize)]
pub struct DecryptedFacilitySecrets {
    pub ein: Option<String>,
    pub bank_routing_number: Option<String>,
    pub bank_account_number: Option<String>,
}

/// Decrypts a facility's `encrypted_secrets` blob -- `facility_id` must
/// be the same value `MappedMerchantAccount::encrypted_secrets` bound it
/// to at write time (the AAD there is just the facility id, unlike a
/// party's `facility_id:role:index`, since `FacilitySecrets` is already
/// 1:1 with the facility).
pub fn decrypt_facility_secrets(
    facility_id: Uuid,
    blob: &[u8],
) -> Result<DecryptedFacilitySecrets, EncryptionError> {
    let plaintext = encryption::decrypt(facility_id.as_bytes(), blob)?;
    serde_json::from_slice(&plaintext)
        .map_err(|_| EncryptionError::Undecryptable("malformed FacilitySecrets plaintext"))
}

/// Decrypted view of the 4 QMS/pinpad system credential fields the
/// Elavon tab's QMS Credentials / Pin Pad Credentials sections show
/// (2026-09-09) -- the read counterpart to `FacilitySecrets::from_fields`'s
/// `account_id`, `qss_web_pin`, `pinpad_user_id`, `qss_api_pin`. Same
/// "revealable on demand" convention `ElavonPartyInfo.ssn` already uses:
/// the real plaintext is returned here, masking (with a Show/Hide
/// toggle) is a frontend concern. Deliberately omits `ein`,
/// `bank_routing_number`, `bank_account_number` (`DecryptedFacilitySecrets`'s
/// own job) and `quikstor_password`/`mid` (no display path anywhere) --
/// same unknown-JSON-keys-silently-dropped trick.
#[derive(Debug, Clone, Deserialize)]
pub struct DecryptedElavonCredentials {
    pub account_id: Option<String>,
    pub qss_web_pin: Option<String>,
    pub pinpad_user_id: Option<String>,
    pub qss_api_pin: Option<String>,
}

/// Decrypts a facility's `encrypted_secrets` blob into just the 4
/// credential fields -- same AAD convention as `decrypt_facility_secrets`
/// (they decrypt the exact same blob, just into different-shaped views).
pub fn decrypt_elavon_credentials(
    facility_id: Uuid,
    blob: &[u8],
) -> Result<DecryptedElavonCredentials, EncryptionError> {
    let plaintext = encryption::decrypt(facility_id.as_bytes(), blob)?;
    serde_json::from_slice(&plaintext)
        .map_err(|_| EncryptionError::Undecryptable("malformed FacilitySecrets plaintext"))
}

/// Masks a bank routing/account number to its last 4 digits, e.g.
/// `104000016` -> `•••••0016`. Non-digit characters (spaces, dashes) are
/// dropped first -- PS stores these as free-typed values, so a real one
/// could in principle carry either. A value with 4 or fewer digits masks
/// entirely rather than showing every digit.
pub fn mask_bank_number(value: &str) -> String {
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() <= 4 {
        return "•".repeat(digits.len().max(4));
    }
    let (masked, visible) = digits.split_at(digits.len() - 4);
    format!("{}{}", "•".repeat(masked.len()), visible)
}
