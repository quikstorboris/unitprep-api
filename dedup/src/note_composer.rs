//! Turns a structured finding (which categories differ, for which
//! group) into the human-facing note text. Kept as a trait specifically
//! so this is swappable: `TemplateNoteComposer` below is the v1,
//! deterministic, no-I/O implementation, matching this project's
//! principles. A future implementation could call out to an LLM for a
//! more tailored, context-aware message instead — same structured
//! input (real field values, unit numbers, tenant names already
//! computed by the rest of this crate), different composition, nothing
//! else in the pipeline needs to change.

use std::collections::HashSet;

use crate::notes::{
    note_template_for_category, NOTE_SEPARATE_TENANTS, NOTE_VERIFY_DIFFERS, NOTE_VERIFY_MATCHES,
};
use crate::types::{FieldCategory, FieldMismatch, TenantGroup, CATEGORY_PRIORITY};

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

        for category in CATEGORY_PRIORITY {
            if differing.iter().any(|m| m.category == category) {
                return note_template_for_category(category).replace("{units}", &units);
            }
        }
        String::new()
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
            "Please update the email address to match across units 101, 204."
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
