//! Resolves a company's legal/display name from Intake + (optional)
//! Merchant Account data -- its own module since the rule draws on both
//! mapping modules' output and is a real, standalone decision worth
//! testing in isolation from the Postgres-writing code that calls it.
//!
//! **Preference order, per Boris (2026-08-31)**: New Merchant Account's
//! own Facility Information (Pre-App) `Legal_Name_2` field, when a
//! Merchant Account run exists at all -- Elavon's own application asks
//! this more carefully than Intake does. Falls back to Intake's own
//! legal-name field when there's no Merchant Account run (not every
//! client uses Elavon).
//!
//! **Sole proprietor exception**: a real, plain-text `Ownership_Type`
//! value observed so far is just `"LLC"` -- matched case-insensitively
//! against "sole prop" rather than one exact string, since the real
//! enum's other values (Sole Proprietorship? Sole Proprietor?) haven't
//! been seen yet. When it matches, the company name becomes
//! `"<Owner's First Last> DBA <Business_DBA>"` instead of `Legal_Name_2`
//! -- small/sole-prop clients often don't have a distinct company legal
//! name from PS's own perspective, so this constructs one worth showing
//! rather than leaving it blank or wrong.
//!
//! **"Legal Name same as DBA?" exception, added 2026-09-08** (real bug:
//! Dubuqueland Mini Storage, Inc. -- Boris): PS's own `Legal_Name?`
//! field answers "Same as Business DBA" for a non-sole-prop client too,
//! and when it does, PS's own form never asks `Legal_Name_2` at all --
//! it comes back `None`, not a copy of the DBA. Previously that fell
//! straight through to Intake's own legal name, which is itself often
//! blank (a non-"first time" sister facility never gets asked its own
//! Corporate Info at all -- see `intake_mapping`'s own doc on that
//! convention), so the company ended up with no legal name whatsoever
//! even though `Business_DBA` had the real answer sitting right there.

use crate::clients::merchant_account_mapping::MappedMerchantAccount;

fn is_sole_proprietor(ownership_type: Option<&str>) -> bool {
    ownership_type
        .map(|value| value.to_lowercase().contains("sole prop"))
        .unwrap_or(false)
}

fn is_same_as_dba(legal_name_same_as_dba: Option<&str>) -> bool {
    legal_name_same_as_dba
        .map(|value| value.to_lowercase().contains("same as"))
        .unwrap_or(false)
}

/// The primary owner's display name -- lowest `party_index` among
/// `party_role == "owner"` parties, matching how `Owner_1` is always
/// entered first in PS's own template.
fn primary_owner_name(merchant_account: &MappedMerchantAccount) -> Option<&str> {
    merchant_account
        .parties
        .iter()
        .filter(|party| party.party_role == "owner")
        .min_by_key(|party| party.party_index)
        .and_then(|party| party.display_name.as_deref())
}

/// Resolves the company name a real "Add to OO" import should use.
/// `intake_legal_name` is `MappedCompany::legal_name` from
/// `intake_mapping` -- passed as a plain `Option<&str>` rather than the
/// whole `MappedCompany`/`MappedIntakeRun` so this module doesn't need
/// to depend on `intake_mapping` at all for one field.
pub fn resolve_company_name(
    intake_legal_name: Option<&str>,
    merchant_account: Option<&MappedMerchantAccount>,
) -> Option<String> {
    if let Some(nma) = merchant_account {
        if is_sole_proprietor(nma.ownership_type.as_deref()) {
            if let (Some(owner), Some(dba)) = (primary_owner_name(nma), nma.business_dba.as_deref()) {
                return Some(format!("{owner} DBA {dba}"));
            }
        }

        if is_same_as_dba(nma.legal_name_same_as_dba.as_deref()) {
            if let Some(dba) = nma.business_dba.as_deref() {
                return Some(dba.to_string());
            }
        }

        if let Some(legal_name) = nma.legal_name.as_deref() {
            return Some(legal_name.to_string());
        }
    }

    intake_legal_name.map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clients::merchant_account_mapping::map_merchant_account_fields;

    const HIGHWAY20_NMA_FIELDS_SANITIZED: &str =
        include_str!("testdata/highway20_merchant_account_fields_sanitized.json");

    fn real_merchant_account() -> MappedMerchantAccount {
        let fields: Vec<crate::process_street::FormField> =
            serde_json::from_str(HIGHWAY20_NMA_FIELDS_SANITIZED)
                .expect("fixture must parse as Vec<FormField>");
        map_merchant_account_fields(&fields)
    }

    #[test]
    fn prefers_merchant_account_legal_name_over_intake_when_not_a_sole_prop() {
        // Highway 20's real Ownership_Type is "LLC", not a sole prop --
        // must use Legal_Name_2 verbatim, not the DBA-naming rule.
        let nma = real_merchant_account();
        let resolved = resolve_company_name(Some("Intake's Own Legal Name"), Some(&nma));
        assert_eq!(resolved.as_deref(), Some("Prairie Enterprises LLC"));
    }

    #[test]
    fn falls_back_to_intake_legal_name_when_no_merchant_account_run_exists() {
        let resolved = resolve_company_name(Some("Only Intake Has This"), None);
        assert_eq!(resolved.as_deref(), Some("Only Intake Has This"));
    }

    #[test]
    fn returns_none_when_neither_source_has_a_legal_name() {
        assert_eq!(resolve_company_name(None, None), None);
    }

    #[test]
    fn sole_proprietor_uses_owner_dba_business_dba_naming() {
        let mut nma = real_merchant_account();
        nma.ownership_type = Some("Sole Proprietorship".to_string());
        // Highway 20's real Owner_1 is "Kyle Lindley" (see
        // merchant_account_mapping's own tests) and its real
        // Business_DBA is "Highway 20 self storage".
        let resolved = resolve_company_name(Some("Ignored When Sole Prop"), Some(&nma));
        assert_eq!(resolved.as_deref(), Some("Kyle Lindley DBA Highway 20 self storage"));
    }

    #[test]
    fn sole_proprietor_match_is_case_insensitive() {
        let mut nma = real_merchant_account();
        nma.ownership_type = Some("SOLE PROP".to_string());
        let resolved = resolve_company_name(None, Some(&nma));
        assert_eq!(resolved.as_deref(), Some("Kyle Lindley DBA Highway 20 self storage"));
    }

    #[test]
    fn sole_proprietor_without_an_owner_or_dba_falls_back_to_legal_name() {
        let mut nma = real_merchant_account();
        nma.ownership_type = Some("Sole Proprietorship".to_string());
        nma.business_dba = None;
        // No Business_DBA -> can't build the DBA name, fall back to
        // whatever legal name is available rather than losing the name
        // entirely.
        let resolved = resolve_company_name(None, Some(&nma));
        assert_eq!(resolved.as_deref(), Some("Prairie Enterprises LLC"));
    }

    #[test]
    fn same_as_dba_uses_business_dba_when_legal_name_2_is_blank() {
        // The real Dubuqueland bug: PS answers Legal_Name? "Same as
        // Business DBA" and, per that same answer, never asks
        // Legal_Name_2 at all -- it comes back None, not a copy of the
        // DBA. Highway 20's real Business_DBA is "Highway 20 self
        // storage".
        let mut nma = real_merchant_account();
        nma.legal_name = None;
        nma.legal_name_same_as_dba = Some("Same as Business DBA".to_string());
        let resolved = resolve_company_name(Some("Ignored When Same As DBA"), Some(&nma));
        assert_eq!(resolved.as_deref(), Some("Highway 20 self storage"));
    }

    #[test]
    fn same_as_dba_match_is_case_insensitive() {
        let mut nma = real_merchant_account();
        nma.legal_name = None;
        nma.legal_name_same_as_dba = Some("SAME AS BUSINESS DBA".to_string());
        let resolved = resolve_company_name(None, Some(&nma));
        assert_eq!(resolved.as_deref(), Some("Highway 20 self storage"));
    }

    #[test]
    fn different_than_dba_does_not_trigger_the_same_as_dba_branch() {
        let mut nma = real_merchant_account();
        nma.legal_name_same_as_dba = Some("Different than Business DBA".to_string());
        // legal_name is still Highway 20's real "Prairie Enterprises
        // LLC" here -- the same_as_dba branch must not fire and steal
        // it.
        let resolved = resolve_company_name(None, Some(&nma));
        assert_eq!(resolved.as_deref(), Some("Prairie Enterprises LLC"));
    }

    #[test]
    fn same_as_dba_with_no_business_dba_falls_back_to_legal_name() {
        let mut nma = real_merchant_account();
        nma.legal_name_same_as_dba = Some("Same as Business DBA".to_string());
        nma.business_dba = None;
        // No DBA to use even though the answer says "same as" -- fall
        // back to legal_name (still populated here) rather than losing
        // the name entirely, same restraint the sole-prop branch uses.
        let resolved = resolve_company_name(None, Some(&nma));
        assert_eq!(resolved.as_deref(), Some("Prairie Enterprises LLC"));
    }
}
