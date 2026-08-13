use super::*;
use crate::note_composer::TemplateNoteComposer;
use crate::types::TenantRecord;

fn group(key: &str, unit: &str, record: TenantRecord) -> TenantGroup {
    TenantGroup {
        key: key.to_string(),
        records: vec![TenantRecord {
            unit_number: unit.to_string(),
            ..record
        }],
    }
}

fn blank() -> TenantRecord {
    TenantRecord::default()
}

#[test]
fn shared_phone_across_different_names_is_surfaced() {
    let a = group(
        "johnsmith",
        "A1",
        TenantRecord {
            first_name: "John".into(),
            last_name: "Smith".into(),
            phone_number: "5551234567".into(),
            ..blank()
        },
    );
    let b = group(
        "janedoe",
        "B2",
        TenantRecord {
            first_name: "Jane".into(),
            last_name: "Doe".into(),
            phone_number: "5551234567".into(),
            ..blank()
        },
    );

    let candidates = find_related_tenant_candidates(&[a, b], &TemplateNoteComposer);

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].evidence.len(), 1);
    assert_eq!(
        candidates[0].evidence[0].signal,
        RelatednessSignal::SharedPhone
    );
    assert_eq!(candidates[0].evidence[0].shared_value, "5551234567");
    assert_eq!(
        candidates[0].group_keys,
        vec!["janedoe".to_string(), "johnsmith".to_string()]
    );
    assert!(candidates[0].note.contains("John Smith"));
    assert!(candidates[0].note.contains("Jane Doe"));
    assert!(candidates[0].note.contains("same phone number"));
}

/// Regression test: phone values used to be normalized as plain
/// case/whitespace-only strings, so the exact same number written with
/// different formatting on each tenant's record would fail to register
/// as shared at all.
#[test]
fn shared_phone_is_surfaced_despite_different_formatting() {
    let a = group(
        "johnsmith",
        "A1",
        TenantRecord {
            first_name: "John".into(),
            last_name: "Smith".into(),
            phone_number: "(555) 123-4567".into(),
            ..blank()
        },
    );
    let b = group(
        "janedoe",
        "B2",
        TenantRecord {
            first_name: "Jane".into(),
            last_name: "Doe".into(),
            phone_number: "555-123-4567".into(),
            ..blank()
        },
    );

    let candidates = find_related_tenant_candidates(&[a, b], &TemplateNoteComposer);

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].evidence.len(), 1);
    assert_eq!(
        candidates[0].evidence[0].signal,
        RelatednessSignal::SharedPhone
    );
    assert_eq!(candidates[0].evidence[0].shared_value, "5551234567");
}

#[test]
fn shared_email_across_different_names_is_surfaced() {
    let a = group(
        "a",
        "A1",
        TenantRecord {
            first_name: "Ann".into(),
            last_name: "Lee".into(),
            email: "shared@example.com".into(),
            ..blank()
        },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord {
            first_name: "Bob".into(),
            last_name: "Ng".into(),
            email: "SHARED@example.com".into(),
            ..blank()
        },
    );

    let candidates = find_related_tenant_candidates(&[a, b], &TemplateNoteComposer);

    let email_evidence: Vec<_> = candidates
        .iter()
        .flat_map(|c| &c.evidence)
        .filter(|e| e.signal == RelatednessSignal::SharedEmail)
        .collect();
    assert_eq!(email_evidence.len(), 1);
    assert_eq!(email_evidence[0].shared_value, "shared@example.com");
}

#[test]
fn shared_alternate_contact_name_across_different_tenants_is_surfaced() {
    let a = group(
        "a",
        "A1",
        TenantRecord {
            first_name: "Ann".into(),
            last_name: "Lee".into(),
            alt_contact_first_name: "Carl".into(),
            alt_contact_last_name: "Reed".into(),
            ..blank()
        },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord {
            first_name: "Bob".into(),
            last_name: "Ng".into(),
            alt_contact_first_name: "Carl".into(),
            alt_contact_last_name: "Reed".into(),
            ..blank()
        },
    );

    let candidates = find_related_tenant_candidates(&[a, b], &TemplateNoteComposer);

    let alt_evidence: Vec<_> = candidates
        .iter()
        .flat_map(|c| &c.evidence)
        .filter(|e| e.signal == RelatednessSignal::SharedAlternateContact)
        .collect();
    assert_eq!(alt_evidence.len(), 1);
    assert_eq!(alt_evidence[0].shared_value, "carl reed");
}

#[test]
fn shared_home_address_across_different_names_is_surfaced() {
    let a = group(
        "a",
        "A1",
        TenantRecord {
            first_name: "Ann".into(),
            last_name: "Lee".into(),
            address_street1: "123 Main St".into(),
            address_city: "Springfield".into(),
            ..blank()
        },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord {
            first_name: "Bob".into(),
            last_name: "Ng".into(),
            address_street1: "123 Main Street".into(),
            address_city: "Springfield".into(),
            ..blank()
        },
    );

    let candidates = find_related_tenant_candidates(&[a, b], &TemplateNoteComposer);

    let address_evidence: Vec<_> = candidates
        .iter()
        .flat_map(|c| &c.evidence)
        .filter(|e| e.signal == RelatednessSignal::SharedHomeAddress)
        .collect();
    assert_eq!(address_evidence.len(), 1);
}

#[test]
fn blank_street_address_never_counts_as_a_shared_address() {
    // Both tenants share a city but neither has a street on file —
    // must not be treated as "sharing an address".
    let a = group(
        "a",
        "A1",
        TenantRecord {
            first_name: "Ann".into(),
            last_name: "Lee".into(),
            address_city: "Springfield".into(),
            ..blank()
        },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord {
            first_name: "Bob".into(),
            last_name: "Ng".into(),
            address_city: "Springfield".into(),
            ..blank()
        },
    );

    let candidates = find_related_tenant_candidates(&[a, b], &TemplateNoteComposer);

    assert!(candidates
        .iter()
        .flat_map(|c| &c.evidence)
        .all(|e| e.signal != RelatednessSignal::SharedHomeAddress));
}

/// Regression test: two tenants whose address data lands in different
/// columns (a real vendor-format inconsistency, not a contrived case --
/// see `full_address`'s doc comment) must not collapse to the same
/// joined string just because dropping blank fields would have made
/// them line up. Here "Springfield" sits in `city` for one tenant and in
/// `street2` for the other, with the fields swapped around it.
#[test]
fn addresses_with_data_shifted_into_different_columns_do_not_falsely_match() {
    let a = group(
        "a",
        "A1",
        TenantRecord {
            first_name: "Ann".into(),
            last_name: "Lee".into(),
            address_street1: "123 Main St".into(),
            address_street2: "".into(),
            address_city: "Springfield".into(),
            ..blank()
        },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord {
            first_name: "Bob".into(),
            last_name: "Ng".into(),
            address_street1: "123 Main St".into(),
            address_street2: "Springfield".into(),
            address_city: "".into(),
            ..blank()
        },
    );

    let candidates = find_related_tenant_candidates(&[a, b], &TemplateNoteComposer);

    assert!(
        candidates
            .iter()
            .flat_map(|c| &c.evidence)
            .all(|e| e.signal != RelatednessSignal::SharedHomeAddress),
        "shifted-column addresses must not register as a shared address just because \
         filtering blanks would have made their joined strings identical"
    );
}

#[test]
fn a_value_shared_by_too_many_tenants_is_not_surfaced() {
    // Four different tenants all sharing the same phone number (e.g. a
    // facility office number reused as a placeholder) is far more
    // likely a data artifact than a real relationship between four
    // specific people — must be excluded, not flagged as one big
    // cluster.
    let groups: Vec<TenantGroup> = ["a", "b", "c", "d"]
        .iter()
        .enumerate()
        .map(|(i, key)| {
            group(
                key,
                &format!("U{i}"),
                TenantRecord {
                    first_name: format!("Name{i}"),
                    phone_number: "5550000000".into(),
                    ..blank()
                },
            )
        })
        .collect();

    let candidates = find_related_tenant_candidates(&groups, &TemplateNoteComposer);

    assert!(candidates
        .iter()
        .flat_map(|c| &c.evidence)
        .all(|e| e.shared_value != "5550000000"));
}

#[test]
fn no_candidates_when_nothing_is_shared() {
    let a = group(
        "a",
        "A1",
        TenantRecord {
            first_name: "Ann".into(),
            last_name: "Lee".into(),
            phone_number: "111".into(),
            ..blank()
        },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord {
            first_name: "Bob".into(),
            last_name: "Ng".into(),
            phone_number: "222".into(),
            ..blank()
        },
    );

    assert!(find_related_tenant_candidates(&[a, b], &TemplateNoteComposer).is_empty());
}

/// Regression test: the literal string `"None"` typed into
/// `AlternateContactLastName` as a stand-in for "no alternate contact"
/// (instead of leaving both name fields blank) must not register as a
/// real shared alternate-contact identity — found on real production
/// data connecting four otherwise-unrelated tenants.
#[test]
fn literal_none_placeholder_never_counts_as_a_shared_alternate_contact() {
    let a = group(
        "a",
        "A1",
        TenantRecord {
            first_name: "Ann".into(),
            last_name: "Lee".into(),
            alt_contact_last_name: "None".into(),
            ..blank()
        },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord {
            first_name: "Bob".into(),
            last_name: "Ng".into(),
            alt_contact_last_name: "none".into(),
            ..blank()
        },
    );

    assert!(find_related_tenant_candidates(&[a, b], &TemplateNoteComposer).is_empty());
}

/// Regression test: a placeholder in an address field (not just a
/// blank street) must not register as a real shared address, and must
/// also not falsely register as a "matching" address elsewhere (the
/// separate-tenants and Muise-typo checks in `phrasing.rs` reuse this
/// same `full_address` gate).
#[test]
fn placeholder_street_address_never_counts_as_a_shared_address() {
    let a = group(
        "a",
        "A1",
        TenantRecord {
            first_name: "Ann".into(),
            last_name: "Lee".into(),
            address_street1: "N/A".into(),
            ..blank()
        },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord {
            first_name: "Bob".into(),
            last_name: "Ng".into(),
            address_street1: "n/a".into(),
            ..blank()
        },
    );

    assert!(
        find_related_tenant_candidates(&[a, b], &TemplateNoteComposer)
            .iter()
            .flat_map(|c| &c.evidence)
            .all(|e| e.signal != RelatednessSignal::SharedHomeAddress)
    );
}

/// Regression test: a short digit fragment (an area-code stub, a
/// truncated paste) sitting in a phone field must not register as a
/// real shared phone number — found on real production data
/// connecting an unrelated tenant to the facility's own account via a
/// 3-digit `AlternateContactPhoneNumber` value.
#[test]
fn a_short_phone_fragment_never_counts_as_a_shared_phone() {
    let a = group(
        "a",
        "A1",
        TenantRecord {
            first_name: "Ann".into(),
            last_name: "Lee".into(),
            alt_contact_phone_number: "978".into(),
            ..blank()
        },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord {
            first_name: "Bob".into(),
            last_name: "Ng".into(),
            alt_contact_phone_number: "978".into(),
            ..blank()
        },
    );

    assert!(find_related_tenant_candidates(&[a, b], &TemplateNoteComposer).is_empty());
}

/// A genuine 10-digit phone number must still be surfaced — the fix
/// above is a length floor, not a blanket exclusion of the phone
/// signal.
#[test]
fn a_full_length_phone_number_is_still_surfaced() {
    let a = group(
        "a",
        "A1",
        TenantRecord {
            first_name: "Ann".into(),
            last_name: "Lee".into(),
            phone_number: "9785551234".into(),
            ..blank()
        },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord {
            first_name: "Bob".into(),
            last_name: "Ng".into(),
            phone_number: "9785551234".into(),
            ..blank()
        },
    );

    let candidates = find_related_tenant_candidates(&[a, b], &TemplateNoteComposer);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].evidence[0].shared_value, "9785551234");
}

/// Regression test: two tenants sharing more than one signal (a
/// spousal pair with identical phone, email, and alternate contact)
/// must merge into ONE household with all evidence listed, not one
/// candidate per signal — the real production pattern (five rows for
/// one obvious pair) this restructuring exists to fix.
#[test]
fn multiple_signals_between_the_same_pair_merge_into_one_household_with_combined_evidence() {
    let a = group(
        "diananelson",
        "B233",
        TenantRecord {
            first_name: "Diana".into(),
            last_name: "Nelson".into(),
            phone_number: "6178774172".into(),
            email: "dbnjazz@gmail.com".into(),
            alt_contact_first_name: "Diana".into(),
            alt_contact_last_name: "Cooper".into(),
            ..blank()
        },
    );
    let b = group(
        "donaldnelson",
        "B109",
        TenantRecord {
            first_name: "Donald".into(),
            last_name: "Nelson".into(),
            phone_number: "6178774172".into(),
            email: "dbnjazz@gmail.com".into(),
            alt_contact_first_name: "Diana".into(),
            alt_contact_last_name: "Cooper".into(),
            ..blank()
        },
    );

    let candidates = find_related_tenant_candidates(&[a, b], &TemplateNoteComposer);

    assert_eq!(
        candidates.len(),
        1,
        "one household, not one candidate per shared field: {candidates:?}"
    );
    assert_eq!(
        candidates[0].group_keys,
        vec!["diananelson".to_string(), "donaldnelson".to_string()]
    );
    assert_eq!(
        candidates[0].evidence.len(),
        3,
        "phone + email + alternate contact"
    );
    assert!(candidates[0].note.contains("Diana Nelson"));
    assert!(candidates[0].note.contains("Donald Nelson"));
    // The combined note names the pair once, not once per signal.
    assert_eq!(candidates[0].note.matches("Diana Nelson").count(), 1);
}

/// Regression test: three tenants chained through two DIFFERENT
/// signals (A shares a phone with B; B shares an email with C; A and
/// C share nothing directly) must merge into one three-member
/// household via transitive closure, not two disjoint pairs a reader
/// would have to notice are actually connected.
#[test]
fn transitively_chained_tenants_merge_into_one_household() {
    let a = group(
        "brucewile",
        "P006",
        TenantRecord {
            first_name: "Bruce".into(),
            last_name: "Wile".into(),
            phone_number: "9787297509".into(),
            ..blank()
        },
    );
    let b = group(
        "robertwiley",
        "B246",
        TenantRecord {
            first_name: "Robert".into(),
            last_name: "Wiley".into(),
            phone_number: "9787297509".into(),
            email: "wileyeng@comcast.net".into(),
            ..blank()
        },
    );
    let c = group(
        "lindawiley",
        "A57",
        TenantRecord {
            first_name: "Linda".into(),
            last_name: "Wiley".into(),
            email: "wileyeng@comcast.net".into(),
            ..blank()
        },
    );

    let candidates = find_related_tenant_candidates(&[a, b, c], &TemplateNoteComposer);

    assert_eq!(
        candidates.len(),
        1,
        "a-b phone + b-c email must merge into one household: {candidates:?}"
    );
    assert_eq!(
        candidates[0].group_keys,
        vec![
            "brucewile".to_string(),
            "lindawiley".to_string(),
            "robertwiley".to_string()
        ]
    );
    assert_eq!(
        candidates[0].evidence.len(),
        2,
        "one phone pair, one email pair"
    );
}

/// A household growing past `MAX_HOUSEHOLD_SIZE` purely through
/// transitive chaining (no single value connects more than
/// `MAX_CLUSTER_SIZE` tenants, but a chain of distinct pairwise values
/// still accretes one big household) must be excluded entirely, not
/// surfaced as one implausibly large "family".
#[test]
fn a_household_exceeding_the_size_cap_via_chaining_is_not_surfaced() {
    // Nine tenants chained pairwise: tenant i and tenant i+1 share a
    // distinct phone value "shared-i" (i in 0..8) via one tenant's
    // PhoneNumber and the next's AlternateContactPhoneNumber -- eight
    // pairwise clusters of size 2 each, well under MAX_CLUSTER_SIZE,
    // but transitively chaining all nine tenants into one household,
    // over MAX_HOUSEHOLD_SIZE (8).
    let groups: Vec<TenantGroup> = (0..9)
        .map(|i| {
            let mut record = TenantRecord {
                first_name: format!("Name{i}"),
                unit_number: format!("U{i}"),
                ..blank()
            };
            if i > 0 {
                record.phone_number = format!("shared-{}", i - 1);
            }
            if i < 8 {
                record.alt_contact_phone_number = format!("shared-{i}");
            }
            TenantGroup {
                key: format!("tenant{i}"),
                records: vec![record],
            }
        })
        .collect();

    let candidates = find_related_tenant_candidates(&groups, &TemplateNoteComposer);

    assert!(
        candidates.is_empty(),
        "a 9-member household chained via 8 distinct, individually-small clusters must be \
         excluded once it exceeds MAX_HOUSEHOLD_SIZE, not surfaced as one giant family: \
         {candidates:?}"
    );
}

#[test]
fn blank_values_never_count_as_shared() {
    // Two tenants who both simply have no phone on file must not be
    // treated as "sharing a blank phone number".
    let a = group(
        "a",
        "A1",
        TenantRecord {
            first_name: "Ann".into(),
            last_name: "Lee".into(),
            ..blank()
        },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord {
            first_name: "Bob".into(),
            last_name: "Ng".into(),
            ..blank()
        },
    );

    assert!(find_related_tenant_candidates(&[a, b], &TemplateNoteComposer).is_empty());
}
