use super::*;
use crate::note_composer::TemplateNoteComposer;
use crate::types::TenantRecord;

fn group(key: &str, unit: &str, record: TenantRecord) -> TenantGroup {
    TenantGroup { key: key.to_string(), records: vec![TenantRecord { unit_number: unit.to_string(), ..record }] }
}

fn blank() -> TenantRecord {
    TenantRecord::default()
}

#[test]
fn shared_phone_across_different_names_is_surfaced() {
    let a = group(
        "johnsmith",
        "A1",
        TenantRecord { first_name: "John".into(), last_name: "Smith".into(), phone_number: "5551234".into(), ..blank() },
    );
    let b = group(
        "janedoe",
        "B2",
        TenantRecord { first_name: "Jane".into(), last_name: "Doe".into(), phone_number: "5551234".into(), ..blank() },
    );

    let candidates = find_related_tenant_candidates(&[a, b], &TemplateNoteComposer);

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].signal, RelatednessSignal::SharedPhone);
    assert_eq!(candidates[0].shared_value, "5551234");
    assert_eq!(candidates[0].group_keys, vec!["janedoe".to_string(), "johnsmith".to_string()]);
    assert!(candidates[0].note.contains("John Smith"));
    assert!(candidates[0].note.contains("Jane Doe"));
    assert!(candidates[0].note.contains("same phone number"));
}

#[test]
fn shared_email_across_different_names_is_surfaced() {
    let a = group(
        "a",
        "A1",
        TenantRecord { first_name: "Ann".into(), last_name: "Lee".into(), email: "shared@example.com".into(), ..blank() },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord { first_name: "Bob".into(), last_name: "Ng".into(), email: "SHARED@example.com".into(), ..blank() },
    );

    let candidates = find_related_tenant_candidates(&[a, b], &TemplateNoteComposer);

    let email_candidates: Vec<_> =
        candidates.iter().filter(|c| c.signal == RelatednessSignal::SharedEmail).collect();
    assert_eq!(email_candidates.len(), 1);
    assert_eq!(email_candidates[0].shared_value, "shared@example.com");
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

    let alt_candidates: Vec<_> =
        candidates.iter().filter(|c| c.signal == RelatednessSignal::SharedAlternateContact).collect();
    assert_eq!(alt_candidates.len(), 1);
    assert_eq!(alt_candidates[0].shared_value, "carl reed");
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

    let address_candidates: Vec<_> =
        candidates.iter().filter(|c| c.signal == RelatednessSignal::SharedHomeAddress).collect();
    assert_eq!(address_candidates.len(), 1);
}

#[test]
fn blank_street_address_never_counts_as_a_shared_address() {
    // Both tenants share a city but neither has a street on file —
    // must not be treated as "sharing an address".
    let a = group(
        "a",
        "A1",
        TenantRecord { first_name: "Ann".into(), last_name: "Lee".into(), address_city: "Springfield".into(), ..blank() },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord { first_name: "Bob".into(), last_name: "Ng".into(), address_city: "Springfield".into(), ..blank() },
    );

    let candidates = find_related_tenant_candidates(&[a, b], &TemplateNoteComposer);

    assert!(candidates.iter().all(|c| c.signal != RelatednessSignal::SharedHomeAddress));
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
                TenantRecord { first_name: format!("Name{i}"), phone_number: "5550000".into(), ..blank() },
            )
        })
        .collect();

    let candidates = find_related_tenant_candidates(&groups, &TemplateNoteComposer);

    assert!(candidates.iter().all(|c| c.shared_value != "5550000"));
}

#[test]
fn no_candidates_when_nothing_is_shared() {
    let a = group(
        "a",
        "A1",
        TenantRecord { first_name: "Ann".into(), last_name: "Lee".into(), phone_number: "111".into(), ..blank() },
    );
    let b = group(
        "b",
        "B2",
        TenantRecord { first_name: "Bob".into(), last_name: "Ng".into(), phone_number: "222".into(), ..blank() },
    );

    assert!(find_related_tenant_candidates(&[a, b], &TemplateNoteComposer).is_empty());
}

#[test]
fn blank_values_never_count_as_shared() {
    // Two tenants who both simply have no phone on file must not be
    // treated as "sharing a blank phone number".
    let a = group("a", "A1", TenantRecord { first_name: "Ann".into(), last_name: "Lee".into(), ..blank() });
    let b = group("b", "B2", TenantRecord { first_name: "Bob".into(), last_name: "Ng".into(), ..blank() });

    assert!(find_related_tenant_candidates(&[a, b], &TemplateNoteComposer).is_empty());
}
