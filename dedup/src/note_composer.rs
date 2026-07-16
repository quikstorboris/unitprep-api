//! Turns a structured finding (which categories differ, for which
//! group) into the human-facing note text. Kept as a trait specifically
//! so this is swappable: `TemplateNoteComposer` below is the v1,
//! deterministic, no-I/O implementation, matching this project's
//! principles. A future implementation could call out to an LLM for a
//! more tailored, context-aware message instead — same structured
//! input (real field values, unit numbers, tenant names already
//! computed by the rest of this crate), different composition, nothing
//! else in the pipeline needs to change.

use std::collections::{BTreeMap, HashSet};

use crate::notes::{
    note_template_for_category, NOTE_SEPARATE_TENANTS, NOTE_VERIFY_DIFFERS, NOTE_VERIFY_MATCHES,
};
use crate::types::{FieldCategory, FieldMismatch, FieldName, TenantGroup, CATEGORY_PRIORITY};

pub trait NoteComposer {
    /// The note for a multi-unit group with at least one contact-info
    /// mismatch. `differing` is always non-empty.
    fn compose_group_note(&self, group: &TenantGroup, differing: &[FieldMismatch]) -> String;

    /// The note for a typo/name-variant candidate — two different
    /// tenant groups whose display names are similar enough to flag.
    fn compose_variant_note(
        &self,
        group_a: &TenantGroup,
        group_b: &TenantGroup,
        contact_info_matches: bool,
    ) -> String;
}

/// Deterministic, template-based composer — no network calls, no
/// randomness, same input always produces the same note.
pub struct TemplateNoteComposer;

impl NoteComposer for TemplateNoteComposer {
    fn compose_group_note(&self, group: &TenantGroup, differing: &[FieldMismatch]) -> String {
        let units = unit_list(group);

        if differing.len() == 1
            && differing[0].category == FieldCategory::Email
            && all_emails_present_and_distinct(group)
        {
            return NOTE_SEPARATE_TENANTS.replace("{units}", &units);
        }

        let lead = CATEGORY_PRIORITY
            .iter()
            .find(|category| differing.iter().any(|m| m.category == **category))
            .map(|category| note_template_for_category(*category).replace("{units}", &units));

        let Some(lead) = lead else {
            return String::new();
        };

        // The lead sentence only names which category (phone, address,
        // alt contact, ...) differs, not which specific field or what
        // the actual values are — someone reading only the exported CSV
        // couldn't tell a missing address from a mismatched phone
        // number without opening the report/UI. This covers every
        // differing category, not just the one the lead sentence is
        // built from, so nothing found by the comparison pass is silently
        // dropped from the note.
        match describe_differing_fields(group, differing) {
            Some(detail) => format!("{lead} Specifically: {detail}."),
            None => lead,
        }
    }

    fn compose_variant_note(
        &self,
        group_a: &TenantGroup,
        group_b: &TenantGroup,
        contact_info_matches: bool,
    ) -> String {
        let template = if contact_info_matches { NOTE_VERIFY_MATCHES } else { NOTE_VERIFY_DIFFERS };
        template
            .replace("{name_a}", &group_a.records[0].display_name())
            .replace("{units_a}", &unit_list(group_a))
            .replace("{name_b}", &group_b.records[0].display_name())
            .replace("{units_b}", &unit_list(group_b))
    }
}

fn unit_list(group: &TenantGroup) -> String {
    let mut units: Vec<&str> = group.records.iter().map(|r| r.unit_number.as_str()).collect();
    units.sort_unstable();
    units.join(", ")
}

/// "`FieldA`: value1 on units X, Y; value2 on unit Z; `FieldB`: ..." —
/// one clause per differing field, across every differing category
/// (not just the lead one), each clause naming which units actually
/// have which value. `None` if there's nothing to describe (shouldn't
/// happen when `differing` is non-empty, but keeps this honest rather
/// than emitting an empty "Specifically: .").
fn describe_differing_fields(group: &TenantGroup, differing: &[FieldMismatch]) -> Option<String> {
    let clauses: Vec<String> = differing
        .iter()
        .flat_map(|mismatch| &mismatch.fields)
        .map(|field_mismatch| describe_field(group, field_mismatch.field))
        .collect();

    if clauses.is_empty() {
        None
    } else {
        Some(clauses.join("; "))
    }
}

/// Groups `group`'s own records by their actual raw value for `field`
/// and names the units on each side — the per-unit attribution a bare
/// distinct-values list (`FieldValueMismatch::values`) doesn't carry.
fn describe_field(group: &TenantGroup, field: FieldName) -> String {
    let mut units_by_value: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for record in &group.records {
        let raw = record.field(field).trim();
        let value = if raw.is_empty() { "(blank)".to_string() } else { raw.to_string() };
        units_by_value.entry(value).or_default().push(record.unit_number.as_str());
    }

    let mut by_value: Vec<(String, Vec<&str>)> = units_by_value.into_iter().collect();
    by_value.sort_by_key(|(value, _)| (value == "(blank)", value.clone()));

    let value_phrases: Vec<String> = by_value
        .into_iter()
        .map(|(value, mut units)| {
            units.sort_unstable();
            let unit_word = if units.len() == 1 { "unit" } else { "units" };
            format!("{value} on {unit_word} {}", units.join(", "))
        })
        .collect();

    format!("{field:?}: {}", value_phrases.join(", "))
}

/// True if every record's email is present (non-blank) and distinct —
/// the "these might just be separate tenants sharing a name" signal,
/// as opposed to a genuine mismatch to fix.
fn all_emails_present_and_distinct(group: &TenantGroup) -> bool {
    let emails: Vec<String> =
        group.records.iter().map(|r| r.email.trim().to_lowercase()).collect();
    if emails.iter().any(|e| e.is_empty()) {
        return false;
    }
    let unique: HashSet<&String> = emails.iter().collect();
    unique.len() == emails.len()
}

#[cfg(test)]
mod tests {
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
    fn group_note_mentions_the_actual_units() {
        // One record has an email, the other is blank — a real mismatch
        // to fix, not two distinct emails (which is the separate-tenants
        // special case, covered by the next test).
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
            "Please update the email address to match across units 101, 204. \
             Specifically: Email: a@example.com on unit 101, (blank) on unit 204."
        );
    }

    #[test]
    fn group_note_details_every_differing_field_with_unit_attribution() {
        // Three units share a category (AltContact) but two separate
        // fields within it differ, one of them three-way — the detail
        // clause should name each field, each distinct value, and
        // exactly which units have it, not just restate the category.
        let mut a = record("D-216", "Carlos Humberto", "Pascual Alejandro", "x@example.com");
        a.alt_contact_first_name = "Carlos".to_string();
        a.alt_contact_phone_number = "3607281619".to_string();
        let mut b = record("S-31", "Carlos Humberto", "Pascual Alejandro", "x@example.com");
        b.alt_contact_first_name = "Agustin".to_string();
        b.alt_contact_phone_number = "3605525629".to_string();
        let mut c = record("S-51", "Carlos Humberto", "Pascual Alejandro", "x@example.com");
        c.alt_contact_first_name = String::new();
        c.alt_contact_phone_number = String::new();

        let group = TenantGroup { key: "carlos".to_string(), records: vec![a, b, c] };

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
        assert_eq!(
            note,
            "Please update the alternate contact info to match across units D-216, S-31, S-51. \
             Specifically: AltContactFirstName: Agustin on unit S-31, Carlos on unit D-216, \
             (blank) on unit S-51; AltContactPhoneNumber: 3605525629 on unit S-31, 3607281619 on \
             unit D-216, (blank) on unit S-51."
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
        assert!(note.contains("may be separate tenants"));
        assert!(note.contains("101, 204"));
    }

    #[test]
    fn variant_note_names_both_tenants_and_units() {
        let group_a = TenantGroup {
            key: "a".to_string(),
            records: vec![record("101", "Warren", "Carolle", "")],
        };
        let group_b = TenantGroup {
            key: "b".to_string(),
            records: vec![record("204", "Warren", "Carroll", "")],
        };

        let note = TemplateNoteComposer.compose_variant_note(&group_a, &group_b, true);
        assert!(note.contains("WARREN CAROLLE"));
        assert!(note.contains("units 101"));
        assert!(note.contains("WARREN CARROLL"));
        assert!(note.contains("units 204"));
        assert!(note.contains("may be the same tenant"));
    }
}
