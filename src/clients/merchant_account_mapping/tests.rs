use uuid::Uuid;

use super::all_sensitive_keys;
use super::mapping::*;
use super::secrets::*;
use crate::clients::encryption::{self};
use crate::process_street::FormField;
use serial_test::serial;

// Real PS field keys/shapes from Prairie Enterprises' Highway 20
// New Merchant Account run, with every sensitive value (SSN, DOB,
// home address, EIN, bank routing/account, QMS system credentials)
// replaced by obvious fakes before this file was ever written to
// disk -- see the vault's Process Street schema doc for how this
// fixture was sanitized. Non-sensitive values (business address,
// rate, application status, real annotation formats) are real.
const HIGHWAY20_NMA_FIELDS_SANITIZED: &str =
    include_str!("../testdata/highway20_merchant_account_fields_sanitized.json");

fn real_fields() -> Vec<FormField> {
    serde_json::from_str(HIGHWAY20_NMA_FIELDS_SANITIZED)
        .expect("fixture must parse as Vec<FormField>")
}

fn set_test_key() {
    std::env::set_var(
        "CLIENT_PII_ENCRYPTION_KEY",
        "1111111111111111111111111111111111111111111111111111111111111111",
    );
}
fn clear_test_key() {
    std::env::remove_var("CLIENT_PII_ENCRYPTION_KEY");
}

#[test]
fn sanitized_snapshot_never_contains_any_sensitive_key_or_its_fake_value() {
    let mapped = map_merchant_account_fields(&real_fields());
    let snapshot_text = mapped.sanitized_snapshot.to_string();

    for key in all_sensitive_keys() {
        assert!(
            !mapped
                .sanitized_snapshot
                .as_object()
                .unwrap()
                .contains_key(&key),
            "sanitized snapshot must never contain the sensitive key {key}"
        );
    }
    // Even the fake placeholder values used in this fixture must
    // never appear -- proves the values were dropped along with
    // their keys, not just renamed.
    assert!(!snapshot_text.contains("FakeTestPassword123"));
    assert!(!snapshot_text.contains("1 Fake Test Lane"));
    assert!(!snapshot_text.contains("000000000")); // fake SSN/EIN/MID
}

#[test]
fn maps_legal_name_dba_and_ownership_type_from_the_real_pre_app_fields() {
    let mapped = map_merchant_account_fields(&real_fields());
    assert_eq!(
        mapped.legal_name.as_deref(),
        Some("Prairie Enterprises LLC")
    );
    assert_eq!(
        mapped.business_dba.as_deref(),
        Some("Highway 20 self storage")
    );
    assert_eq!(mapped.ownership_type.as_deref(), Some("LLC"));
}

#[test]
fn maps_ein_last_4_and_business_address_from_the_real_pre_app_fields() {
    let mapped = map_merchant_account_fields(&real_fields());

    // Fixture's own fake EIN is "111111111" -- mask_bank_number's
    // own last-4 rule applies unchanged.
    assert_eq!(mapped.ein_last_4.as_deref(), Some("•••••1111"));
    assert_eq!(
        mapped.business_address.as_deref(),
        Some("1030 East Grant Highway, Marengo, Il 60152")
    );
}

#[test]
fn ein_last_4_is_none_when_ein_was_never_answered() {
    let fields: Vec<FormField> = real_fields()
        .into_iter()
        .filter(|f| f.key != "EIN")
        .collect();

    let mapped = map_merchant_account_fields(&fields);

    assert_eq!(mapped.ein_last_4, None);
}

#[test]
fn combine_address_is_none_when_every_part_is_missing() {
    assert_eq!(combine_address(None, None, None, None), None);
}

#[test]
fn combine_address_handles_a_street_with_no_city_state_or_zip() {
    assert_eq!(
        combine_address(
            Some("1030 East Grant Highway".to_string()),
            None,
            None,
            None
        ),
        Some("1030 East Grant Highway".to_string())
    );
}

#[test]
fn combine_address_handles_city_state_zip_with_no_street() {
    assert_eq!(
        combine_address(
            None,
            Some("Marengo".to_string()),
            Some("IL".to_string()),
            Some("60152".to_string())
        ),
        Some("Marengo, IL 60152".to_string())
    );
}

#[test]
fn sanitized_snapshot_keeps_non_sensitive_business_data() {
    let mapped = map_merchant_account_fields(&real_fields());
    let snapshot_text = mapped.sanitized_snapshot.to_string();
    assert!(snapshot_text.contains("Prairie Enterprises"));
    assert!(mapped
        .sanitized_snapshot
        .as_object()
        .unwrap()
        .contains_key("Business_DBA"));
}

#[test]
fn maps_three_real_owners_and_skips_the_blank_fourth_and_signer() {
    let mapped = map_merchant_account_fields(&real_fields());
    let owners: Vec<_> = mapped
        .parties
        .iter()
        .filter(|p| p.party_role == "owner")
        .collect();
    assert_eq!(
        owners.len(),
        3,
        "owner 4 was blank on this real run and must be skipped"
    );
    assert_eq!(owners[0].display_name.as_deref(), Some("Kyle Lindley"));
    assert_eq!(owners[0].ownership_percent, Some(30.0));

    assert!(
        !mapped.parties.iter().any(|p| p.party_role == "signer"),
        "the signer slot was entirely blank on this real run and must be skipped"
    );
}

#[test]
#[serial(client_pii_encryption_key_env)]
fn encrypts_and_round_trips_a_real_owners_pii() {
    set_test_key();
    let mapped = map_merchant_account_fields(&real_fields());
    let owner = mapped
        .parties
        .iter()
        .find(|p| p.party_role == "owner" && p.party_index == 1)
        .unwrap();

    let facility_id = Uuid::new_v4();
    let blob = owner
        .encrypted_pii(facility_id)
        .expect("encryption must succeed")
        .expect("owner 1 has real PII on this fixture");

    let aad = format!("{facility_id}:owner:1");
    let decrypted = encryption::decrypt(aad.as_bytes(), &blob).expect("decryption must succeed");
    let pii: PartyPii = serde_json::from_slice(&decrypted).unwrap();
    assert_eq!(pii.ssn.as_deref(), Some("000000000")); // the fixture's fake SSN
    clear_test_key();
}

#[test]
#[serial(client_pii_encryption_key_env)]
fn a_partys_pii_does_not_decrypt_under_a_different_partys_aad() {
    set_test_key();
    let mapped = map_merchant_account_fields(&real_fields());
    let owner1 = mapped
        .parties
        .iter()
        .find(|p| p.party_index == 1 && p.party_role == "owner")
        .unwrap();

    let facility_id = Uuid::new_v4();
    let blob = owner1.encrypted_pii(facility_id).unwrap().unwrap();

    let wrong_aad = format!("{facility_id}:owner:2");
    assert!(
        encryption::decrypt(wrong_aad.as_bytes(), &blob).is_err(),
        "owner 1's ciphertext must not decrypt under owner 2's AAD, even within the same facility"
    );
    clear_test_key();
}

#[test]
fn intermediary_business_parties_have_no_encrypted_pii() {
    let mapped = map_merchant_account_fields(&real_fields());
    let biz = mapped
        .parties
        .iter()
        .find(|p| p.party_role == "intermediary_business")
        .expect("this fixture has one real intermediary business");
    assert_eq!(biz.encrypted_pii(Uuid::new_v4()).unwrap(), None);
}

#[test]
#[serial(client_pii_encryption_key_env)]
fn facility_secrets_encrypt_and_round_trip() {
    set_test_key();
    let mapped = map_merchant_account_fields(&real_fields());
    let facility_id = Uuid::new_v4();
    let blob = mapped
        .encrypted_secrets(facility_id)
        .expect("encryption must succeed")
        .expect("this fixture has real (fake-value) secrets");

    let decrypted = encryption::decrypt(facility_id.as_bytes(), &blob).unwrap();
    let secrets: FacilitySecrets = serde_json::from_slice(&decrypted).unwrap();
    assert_eq!(secrets.ein.as_deref(), Some("111111111"));
    assert_eq!(
        secrets.quikstor_password.as_deref(),
        Some("FakeTestPassword123!")
    );
    clear_test_key();
}

#[test]
fn maps_revenue_and_volume_fields_from_the_real_pre_app_fields() {
    let mapped = map_merchant_account_fields(&real_fields());
    assert_eq!(
        mapped.total_annual_business_revenue_raw.as_deref(),
        Some("840000")
    );
    assert_eq!(mapped.total_monthly_sales_raw.as_deref(), Some("70000"));
    assert_eq!(
        mapped.average_credit_card_payment_amount_raw.as_deref(),
        Some("150")
    );
    assert_eq!(
        mapped.highest_credit_card_payment_amount_raw.as_deref(),
        Some("2000")
    );
    assert_eq!(
        mapped.high_cc_payment_times_per_year_raw.as_deref(),
        Some("25")
    );
    assert_eq!(mapped.offers_ach_raw.as_deref(), Some("Yes"));
    assert_eq!(
        mapped.annual_electronic_check_volume_raw.as_deref(),
        Some("20000")
    );
    assert_eq!(
        mapped.average_electronic_check_amount_raw.as_deref(),
        Some("150")
    );
    assert_eq!(
        mapped.maximum_electronic_check_amount_raw.as_deref(),
        Some("1500")
    );
}

#[test]
#[serial(client_pii_encryption_key_env)]
fn decrypts_facility_secrets_to_ein_and_bank_numbers_only() {
    set_test_key();
    let mapped = map_merchant_account_fields(&real_fields());
    let facility_id = Uuid::new_v4();
    let blob = mapped.encrypted_secrets(facility_id).unwrap().unwrap();

    let secrets = decrypt_facility_secrets(facility_id, &blob).expect("decryption must succeed");
    assert_eq!(secrets.ein.as_deref(), Some("111111111"));
    assert_eq!(secrets.bank_routing_number.as_deref(), Some("011000015"));
    assert_eq!(secrets.bank_account_number.as_deref(), Some("999999999"));
    clear_test_key();
}

#[test]
fn mask_bank_number_shows_only_the_last_4_digits() {
    assert_eq!(mask_bank_number("104000016"), "•••••0016");
    assert_eq!(mask_bank_number("515612"), "••5612");
    assert_eq!(mask_bank_number("12"), "••••");
    assert_eq!(mask_bank_number("104-000-016"), "•••••0016");
}

#[test]
#[serial(client_pii_encryption_key_env)]
fn decrypts_elavon_credentials_to_the_4_qms_pinpad_fields_only() {
    set_test_key();
    let mapped = map_merchant_account_fields(&real_fields());
    let facility_id = Uuid::new_v4();
    let blob = mapped.encrypted_secrets(facility_id).unwrap().unwrap();

    let credentials =
        decrypt_elavon_credentials(facility_id, &blob).expect("decryption must succeed");
    assert_eq!(credentials.account_id.as_deref(), Some("0000000"));
    assert_eq!(
        credentials.qss_web_pin.as_deref(),
        Some("FAKEWEBPINFAKEWEBPINFAKEWEBPINFAKEWEBPINFAKEWEBPINFAKEWEBPIN00")
    );
    assert_eq!(credentials.pinpad_user_id.as_deref(), Some("FAKEPINPADID"));
    assert_eq!(
        credentials.qss_api_pin.as_deref(),
        Some("FAKEAPIPINFAKEAPIPINFAKEAPIPINFAKEAPIPINFAKEAPIPINFAKEAPIPIN000")
    );
    clear_test_key();
}

/// G1: a stray `{:?}` of the decrypted credentials must not leak the PINs.
#[test]
fn decrypted_elavon_credentials_redact_the_pins_but_not_the_ids() {
    const SECRET: &str = "TOPSECRET-do-not-log";
    let credentials = DecryptedElavonCredentials {
        account_id: Some("acct-9".to_string()),
        qss_web_pin: Some(SECRET.to_string()),
        pinpad_user_id: Some("pinpad-user".to_string()),
        qss_api_pin: Some(SECRET.to_string()),
    };
    let printed = format!("{credentials:?}");
    assert!(!printed.contains(SECRET), "{printed}");
    assert!(printed.contains("acct-9") && printed.contains("pinpad-user"));
}
