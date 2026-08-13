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
fn distinct_emails_and_distinct_addresses_suggests_separate_tenants() {
    let mut a = record("101", "John", "Smith", "a@example.com");
    a.address_street1 = "12 Elm St".to_string();
    a.address_city = "Springfield".to_string();
    let mut b = record("204", "John", "Smith", "b@example.com");
    b.address_street1 = "88 Oak Ave".to_string();
    b.address_city = "Shelbyville".to_string();

    let group = TenantGroup {
        key: "smith".to_string(),
        records: vec![a, b],
    };
    let differing = vec![
        FieldMismatch {
            category: FieldCategory::Email,
            fields: vec![FieldValueMismatch {
                field: crate::types::FieldName::Email,
                values: vec!["a@example.com".into(), "b@example.com".into()],
            }],
        },
        FieldMismatch {
            category: FieldCategory::Address,
            fields: vec![FieldValueMismatch {
                field: crate::types::FieldName::AddressStreet1,
                values: vec!["12 Elm St".into(), "88 Oak Ave".into()],
            }],
        },
    ];

    let note = TemplateNoteComposer.compose_group_note(&group, &differing);
    assert!(note.starts_with("Units 101 and 204 share a name"));
    assert!(note.contains("may be separate tenants"));
}

/// Regression test for a real bug found on production data: two units
/// belonging to the same person (identical address, one-character email
/// typo) were told they "may be separate tenants" — because the old
/// condition only checked that Email was the sole differing category,
/// which (since a matching category never appears in `differing`)
/// actually *required* the address to already match. Distinct emails
/// with a *matching* address must fall through to the plain
/// "update the email to match" note instead.
#[test]
fn distinct_emails_with_matching_address_is_a_typo_not_separate_tenants() {
    let mut a = record("B207", "Christopher", "Muise", "mammamoose7@gmail.com");
    a.address_street1 = "26 Campmeeting Rd".to_string();
    a.address_city = "Topsfield".to_string();
    let mut b = record("B256", "Christopher", "Muise", "mommamoose7@gmail.com");
    b.address_street1 = "26 Campmeeting Rd".to_string();
    b.address_city = "Topsfield".to_string();

    let group = TenantGroup {
        key: "muise".to_string(),
        records: vec![a, b],
    };
    let differing = vec![FieldMismatch {
        category: FieldCategory::Email,
        fields: vec![FieldValueMismatch {
            field: crate::types::FieldName::Email,
            values: vec![
                "mammamoose7@gmail.com".into(),
                "mommamoose7@gmail.com".into(),
            ],
        }],
    }];

    let note = TemplateNoteComposer.compose_group_note(&group, &differing);
    assert!(!note.contains("may be separate tenants"));
    assert_eq!(
        note,
        "Please update the email address to match across units B207 and B256. \
         Email address is mammamoose7@gmail.com for unit B207 and \
         mommamoose7@gmail.com for unit B256."
    );
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

/// Real Rowley Self Storage shape: same tenant, matching address AND
/// phone, only email differs by one character — the note should name
/// this outright as likely a typo rather than leaving the reader to
/// notice the matching address/phone on their own.
#[test]
fn email_mismatch_with_matching_address_and_phone_gets_the_likely_typo_sentence() {
    let mut a = record("B207", "Christopher", "Muise", "mommamoose7@gmail.com");
    a.address_street1 = "26 Campmeeting Rd".to_string();
    a.address_city = "Topsfield".to_string();
    a.phone_number = "9785004160".to_string();
    let mut b = record("B256", "Christopher", "Muise", "mammamoose7@gmail.com");
    b.address_street1 = "26 Campmeeting Rd".to_string();
    b.address_city = "Topsfield".to_string();
    b.phone_number = "9785004160".to_string();

    let group = TenantGroup {
        key: "muise".to_string(),
        records: vec![a, b],
    };
    let differing = vec![FieldMismatch {
        category: FieldCategory::Email,
        fields: vec![FieldValueMismatch {
            field: crate::types::FieldName::Email,
            values: vec![
                "mommamoose7@gmail.com".into(),
                "mammamoose7@gmail.com".into(),
            ],
        }],
    }];

    let note = TemplateNoteComposer.compose_group_note(&group, &differing);
    assert!(!note.contains("may be separate tenants"));
    assert!(
        note.contains("The matching address and phone suggest this is one person"),
        "note should name the typo explicitly when address and phone both already match: \
         {note}"
    );
}

/// The same email-only mismatch, but with the address blank on both
/// units — there's no corroborating evidence, so the note must NOT
/// claim a likely typo just because nothing else differs either.
#[test]
fn email_mismatch_without_a_matching_address_does_not_get_the_likely_typo_sentence() {
    let a = record("101", "John", "Smith", "a@example.com");
    let b = record("204", "John", "Smith", "b.typo@example.com");

    let group = TenantGroup {
        key: "smith".to_string(),
        records: vec![a, b],
    };
    let differing = vec![FieldMismatch {
        category: FieldCategory::Email,
        fields: vec![FieldValueMismatch {
            field: crate::types::FieldName::Email,
            values: vec!["a@example.com".into(), "b.typo@example.com".into()],
        }],
    }];

    let note = TemplateNoteComposer.compose_group_note(&group, &differing);
    assert!(!note.contains("The matching address and phone suggest"));
}

#[test]
fn relatedness_note_with_a_single_piece_of_evidence_uses_the_original_wording() {
    let a = TenantGroup {
        key: "a".to_string(),
        records: vec![record("A1", "John", "Smith", "")],
    };
    let b = TenantGroup {
        key: "b".to_string(),
        records: vec![record("B2", "Jane", "Doe", "")],
    };

    let evidence = vec![RelatednessEvidenceInput {
        signal: RelatednessSignal::SharedPhone,
        shared_value: "5551234",
        member_groups: vec![&a, &b],
    }];

    let note = TemplateNoteComposer.compose_relatedness_note(&evidence);
    assert_eq!(
        note,
        "John Smith (unit A1) and Jane Doe (unit B2) share the same phone number (5551234) \
         despite having different names — worth checking whether these are related tenants."
    );
}

/// The real-world case this restructuring exists for: one pair of
/// tenants sharing three different signals must produce ONE combined
/// sentence naming the pair once, not three separate clauses each
/// repeating "John Smith and Jane Doe".
#[test]
fn relatedness_note_combines_multiple_signals_for_the_same_pair_into_one_clause() {
    let a = TenantGroup {
        key: "a".to_string(),
        records: vec![record("A1", "John", "Smith", "")],
    };
    let b = TenantGroup {
        key: "b".to_string(),
        records: vec![record("B2", "Jane", "Doe", "")],
    };

    let evidence = vec![
        RelatednessEvidenceInput {
            signal: RelatednessSignal::SharedPhone,
            shared_value: "5551234",
            member_groups: vec![&a, &b],
        },
        RelatednessEvidenceInput {
            signal: RelatednessSignal::SharedEmail,
            shared_value: "shared@example.com",
            member_groups: vec![&a, &b],
        },
    ];

    let note = TemplateNoteComposer.compose_relatedness_note(&evidence);
    assert_eq!(note.matches("John Smith").count(), 1, "note: {note}");
    assert!(note.contains("phone number (5551234)"));
    assert!(note.contains("email address (shared@example.com)"));
    assert!(note.ends_with("worth checking whether these are related tenants."));
}

/// Two pieces of evidence connecting DIFFERENT subsets of a
/// three-tenant household (A-B via phone, B-C via email) must produce
/// two clauses, each naming only the pair it actually applies to —
/// not implying A and C share something they don't.
#[test]
fn relatedness_note_keeps_evidence_for_different_subsets_in_separate_clauses() {
    let a = TenantGroup {
        key: "a".to_string(),
        records: vec![record("P006", "Bruce", "Wile", "")],
    };
    let b = TenantGroup {
        key: "b".to_string(),
        records: vec![record("B246", "Robert", "Wiley", "")],
    };
    let c = TenantGroup {
        key: "c".to_string(),
        records: vec![record("A57", "Linda", "Wiley", "")],
    };

    let evidence = vec![
        RelatednessEvidenceInput {
            signal: RelatednessSignal::SharedPhone,
            shared_value: "9787297509",
            member_groups: vec![&a, &b],
        },
        RelatednessEvidenceInput {
            signal: RelatednessSignal::SharedEmail,
            shared_value: "wileyeng@comcast.net",
            member_groups: vec![&b, &c],
        },
    ];

    let note = TemplateNoteComposer.compose_relatedness_note(&evidence);
    assert!(note.contains(
        "Bruce Wile (unit P006) and Robert Wiley (unit B246) share the same phone number \
         (9787297509)"
    ));
    assert!(note.contains(
        "Robert Wiley (unit B246) and Linda Wiley (unit A57) share the same email address \
         (wileyeng@comcast.net)"
    ));
    // Each clause names only its own pair — Bruce and Linda never
    // appear together in the same clause, since they share nothing
    // directly.
    assert_eq!(note.matches("Bruce Wile").count(), 1);
    assert_eq!(note.matches("Linda Wiley").count(), 1);
}
