use super::*;
use crate::types::{FieldCategory, FieldValueMismatch};

fn record(unit: &str, first: &str, last: &str, email: &str) -> crate::types::TenantRecord {
    crate::types::TenantRecord {
        unit_number: unit.to_string(),
        first_name: first.to_string(),
        last_name: last.to_string(),
        email: email.to_string(),
        ..Default::default()
    }
}

#[test]
fn group_note_mentions_the_actual_units_with_correct_number_agreement() {
    // One record has an email, the other is blank — a real mismatch
    // to fix, not two distinct emails (which is the separate-tenants
    // special case, covered by a test below).
    let group = TenantGroup {
        key: "smith".to_string(),
        records: vec![
            record("101", "John", "Smith", "a@example.com"),
            record("204", "John", "Smith", ""),
        ],
    };
    let differing = vec![FieldMismatch {
        category: FieldCategory::Email,
        fields: vec![FieldValueMismatch {
            field: crate::types::FieldName::Email,
            values: vec!["a@example.com".into(), "(blank)".into()],
        }],
    }];

    let note = TemplateNoteComposer.compose_group_note(&group, &differing);
    assert_eq!(
        note,
        "Please update the email address to match across units 101 and 204. \
         Email address is a@example.com for unit 101, but blank for unit 204."
    );
}

#[test]
fn group_note_details_every_differing_field_as_plain_sentences() {
    // Three units share a category (AltContact) but two separate
    // fields within it differ, one of them three-way — the detail
    // text should name each field, each distinct value, and exactly
    // which units have it, in plain English, not just restate the
    // category.
    let mut a = record(
        "D-216",
        "Carlos Humberto",
        "Pascual Alejandro",
        "x@example.com",
    );
    a.alt_contact_first_name = "Carlos".to_string();
    a.alt_contact_phone_number = "3607281619".to_string();
    let mut b = record(
        "S-31",
        "Carlos Humberto",
        "Pascual Alejandro",
        "x@example.com",
    );
    b.alt_contact_first_name = "Agustin".to_string();
    b.alt_contact_phone_number = "3605525629".to_string();
    let mut c = record(
        "S-51",
        "Carlos Humberto",
        "Pascual Alejandro",
        "x@example.com",
    );
    c.alt_contact_first_name = String::new();
    c.alt_contact_phone_number = String::new();

    let group = TenantGroup {
        key: "carlos".to_string(),
        records: vec![a, b, c],
    };

    let differing = vec![FieldMismatch {
        category: FieldCategory::AltContact,
        fields: vec![
            FieldValueMismatch {
                field: crate::types::FieldName::AltContactFirstName,
                values: vec!["Agustin".into(), "Carlos".into(), "(blank)".into()],
            },
            FieldValueMismatch {
                field: crate::types::FieldName::AltContactPhoneNumber,
                values: vec!["3605525629".into(), "3607281619".into(), "(blank)".into()],
            },
        ],
    }];

    let note = TemplateNoteComposer.compose_group_note(&group, &differing);
    // Clauses sort alphabetically by value (blank last), not by
    // insertion/unit order — "Agustin" sorts before "Carlos".
    assert_eq!(
        note,
        "Please update the alternate contact info to match across units D-216, S-31, and S-51. \
         Alternate contact first name is Agustin for unit S-31, Carlos for unit D-216, but \
         blank for unit S-51. Alternate contact phone number is 3605525629 for unit S-31, \
         3607281619 for unit D-216, but blank for unit S-51."
    );
}

#[test]
fn describe_group_bullets_returns_one_sentence_per_field() {
    let group = TenantGroup {
        key: "smith".to_string(),
        records: vec![
            record("101", "John", "Smith", "a@example.com"),
            record("204", "John", "Smith", ""),
        ],
    };
    let differing = vec![FieldMismatch {
        category: FieldCategory::Email,
        fields: vec![FieldValueMismatch {
            field: crate::types::FieldName::Email,
            values: vec!["a@example.com".into(), "(blank)".into()],
        }],
    }];

    let bullets = TemplateNoteComposer.describe_group_bullets(&group, &differing);
    assert_eq!(bullets.len(), 1);
    assert_eq!(bullets[0].0, crate::types::FieldName::Email);
    assert_eq!(
        bullets[0].1,
        "Email address is a@example.com for unit 101, but blank for unit 204."
    );
}

#[test]
fn distinct_emails_only_suggests_separate_tenants() {
    let group = TenantGroup {
        key: "smith".to_string(),
        records: vec![
            record("101", "John", "Smith", "a@example.com"),
            record("204", "John", "Smith", "b@example.com"),
        ],
    };
    let differing = vec![FieldMismatch {
        category: FieldCategory::Email,
        fields: vec![FieldValueMismatch {
            field: crate::types::FieldName::Email,
            values: vec!["a@example.com".into(), "b@example.com".into()],
        }],
    }];

    let note = TemplateNoteComposer.compose_group_note(&group, &differing);
    assert!(note.starts_with("Units 101 and 204 share a name"));
    assert!(note.contains("may be separate tenants"));
}

#[test]
fn variant_note_names_both_tenants_and_units_in_title_case() {
    let group_a = TenantGroup {
        key: "a".to_string(),
        records: vec![record("101", "Warren", "Carolle", "")],
    };
    let group_b = TenantGroup {
        key: "b".to_string(),
        records: vec![record("204", "Warren", "Carroll", "")],
    };

    let note = TemplateNoteComposer.compose_variant_note(&group_a, &group_b, true);
    assert!(note.contains("Warren Carolle"));
    assert!(note.contains("unit 101"));
    assert!(note.contains("Warren Carroll"));
    assert!(note.contains("unit 204"));
    assert!(note.contains("may be the same tenant"));
}

#[test]
fn variant_note_uses_plural_units_when_a_side_has_more_than_one() {
    let group_a = TenantGroup {
        key: "a".to_string(),
        records: vec![
            record("101", "Warren", "Carolle", ""),
            record("102", "Warren", "Carolle", ""),
        ],
    };
    let group_b = TenantGroup {
        key: "b".to_string(),
        records: vec![record("204", "Warren", "Carroll", "")],
    };

    let note = TemplateNoteComposer.compose_variant_note(&group_a, &group_b, true);
    assert!(note.contains("units 101 and 102"));
    assert!(note.contains("unit 204"));
}
