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
    note_template_for_category, relatedness_template_for_signal, NOTE_SEPARATE_TENANTS,
    NOTE_VERIFY_DIFFERS, NOTE_VERIFY_MATCHES,
};
use crate::relatedness::RelatednessSignal;
use crate::types::{FieldCategory, FieldMismatch, FieldName, TenantGroup, CATEGORY_PRIORITY};

pub trait NoteComposer {
    /// The note for a multi-unit group with at least one contact-info
    /// mismatch. `differing` is always non-empty.
    fn compose_group_note(&self, group: &TenantGroup, differing: &[FieldMismatch]) -> String;

    /// The same finding as `compose_group_note`, but as one plain-English
    /// sentence per differing field instead of a single paragraph —
    /// what a UI renders as individual bullets. `compose_group_note`'s
    /// own detail text is built from exactly these sentences, so the two
    /// can never drift apart in phrasing.
    fn describe_group_bullets(
        &self,
        group: &TenantGroup,
        differing: &[FieldMismatch],
    ) -> Vec<(FieldName, String)>;

    /// The note for a typo/name-variant candidate — two different
    /// tenant groups whose display names are similar enough to flag.
    fn compose_variant_note(
        &self,
        group_a: &TenantGroup,
        group_b: &TenantGroup,
        contact_info_matches: bool,
    ) -> String;

    /// The note for a related-tenant candidate — two or more tenant
    /// groups (different name keys) sharing a specific, non-blank
    /// value (`shared_value`) under `signal`. `groups` always has at
    /// least 2 entries.
    fn compose_relatedness_note(
        &self,
        groups: &[&TenantGroup],
        signal: RelatednessSignal,
        shared_value: &str,
    ) -> String;
}

/// Deterministic, template-based composer — no network calls, no
/// randomness, same input always produces the same note.
pub struct TemplateNoteComposer;

impl NoteComposer for TemplateNoteComposer {
    fn compose_group_note(&self, group: &TenantGroup, differing: &[FieldMismatch]) -> String {
        let units = units_phrase(&group_units(group));

        if differing.len() == 1
            && differing[0].category == FieldCategory::Email
            && all_emails_present_and_distinct(group)
        {
            return capitalize_first(&NOTE_SEPARATE_TENANTS.replace("{units}", &units));
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
        // dropped from the note. Built from the exact same sentences
        // `describe_group_bullets` returns, so the flat note and the
        // structured bullets a UI renders can never say different things.
        let bullets = self.describe_group_bullets(group, differing);
        if bullets.is_empty() {
            return lead;
        }

        let detail = bullets
            .iter()
            .map(|(_, sentence)| sentence.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        format!("{lead} {detail}")
    }

    fn describe_group_bullets(
        &self,
        group: &TenantGroup,
        differing: &[FieldMismatch],
    ) -> Vec<(FieldName, String)> {
        differing
            .iter()
            .flat_map(|mismatch| &mismatch.fields)
            .map(|field_mismatch| (field_mismatch.field, describe_field(group, field_mismatch.field)))
            .collect()
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
            .replace("{units_a}", &units_phrase(&group_units(group_a)))
            .replace("{name_b}", &group_b.records[0].display_name())
            .replace("{units_b}", &units_phrase(&group_units(group_b)))
    }

    fn compose_relatedness_note(
        &self,
        groups: &[&TenantGroup],
        signal: RelatednessSignal,
        shared_value: &str,
    ) -> String {
        let names = groups
            .iter()
            .map(|g| format!("{} ({})", g.records[0].display_name(), units_phrase(&group_units(g))))
            .collect::<Vec<_>>()
            .join(" and ");

        relatedness_template_for_signal(signal)
            .replace("{names}", &names)
            .replace("{value}", shared_value)
    }
}

/// This group's unit numbers, sorted. The base list `units_phrase` turns
/// into a properly-worded phrase — kept separate so callers that need
/// the raw list (e.g. the API layer building a `units: Vec<String>`
/// field for the frontend) don't have to parse a phrase back apart.
pub fn group_units(group: &TenantGroup) -> Vec<&str> {
    let mut units: Vec<&str> = group.records.iter().map(|r| r.unit_number.as_str()).collect();
    units.sort_unstable();
    units
}

/// `"unit 13"` for one, `"units 54, 67, and 77"` (Oxford comma) for more
/// — every note template's `{units}`/`{units_a}`/`{units_b}` placeholder
/// is substituted with exactly this, so "unit"/"units" always agrees
/// with how many are actually listed instead of the template hardcoding
/// the plural.
pub fn units_phrase(units: &[&str]) -> String {
    match units {
        [] => String::new(),
        [one] => format!("unit {one}"),
        many => format!("units {}", oxford_join(many)),
    }
}

/// `"A"` / `"A and B"` / `"A, B, and C"` — a comma before the final
/// "and" only once there are 3+ items, matching normal English (nobody
/// writes "A, and B").
fn oxford_join(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => one.to_string(),
        [a, b] => format!("{a} and {b}"),
        _ => {
            let (last, rest) = items.split_last().expect("non-empty slice");
            format!("{}, and {last}", rest.join(", "))
        }
    }
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A friendly label for a field, for a UI/note reader who's never heard
/// of this crate's internal `FieldName` — used in place of the raw Rust
/// debug string (`AltContactFirstName`) the note text used to show.
pub fn human_label(field: FieldName) -> &'static str {
    match field {
        FieldName::PhoneNumber => "Phone number",
        FieldName::PhoneNumberPrefix => "Phone country code",
        FieldName::Email => "Email address",
        FieldName::AddressStreet1 => "Street address",
        FieldName::AddressStreet2 => "Street address (line 2)",
        FieldName::AddressCity => "City",
        FieldName::AddressState => "State",
        FieldName::AddressPostalCode => "ZIP code",
        FieldName::AltContactFirstName => "Alternate contact first name",
        FieldName::AltContactLastName => "Alternate contact last name",
        FieldName::AltContactEmail => "Alternate contact email address",
        FieldName::AltContactPhoneNumber => "Alternate contact phone number",
        FieldName::AltContactPhoneNumberPrefix => "Alternate contact phone country code",
        FieldName::AltContactAddressStreet1 => "Alternate contact street address",
        FieldName::AltContactAddressStreet2 => "Alternate contact street address (line 2)",
        FieldName::AltContactAddressCity => "Alternate contact city",
        FieldName::AltContactAddressState => "Alternate contact state",
        FieldName::AltContactAddressPostalCode => "Alternate contact ZIP code",
        FieldName::CompanyName => "Company name",
        FieldName::FirstName => "First name",
        FieldName::LastName => "Last name",
    }
}

/// One plain sentence for a single differing field — e.g. "Phone number
/// is (618) 313-1505 for units 54, 67, and 77, but blank for unit 13."
/// Groups `group`'s own records by their actual raw value for `field`
/// and names the units on each side — the per-unit attribution a bare
/// distinct-values list (`FieldValueMismatch::values`) doesn't carry.
fn describe_field(group: &TenantGroup, field: FieldName) -> String {
    let by_value = units_by_value(group, field);

    let clauses: Vec<String> = by_value
        .iter()
        .map(|(value, units)| {
            let value_text = if value == "(blank)" { "blank" } else { value };
            format!("{value_text} for {}", units_phrase(units))
        })
        .collect();

    // Blank sorts last (see `units_by_value`) — when the final value is
    // blank, "but" reads as the natural contrast ("X, but blank for Y")
    // rather than "and" (which reads fine joining two present values,
    // odd joining a value against its own absence).
    let last_is_blank = by_value.last().is_some_and(|(value, _)| value == "(blank)");
    let connector = if last_is_blank { "but" } else { "and" };

    // A comma always precedes a contrastive "but"; "and" only gets one
    // once there are 3+ clauses (Oxford comma), matching `oxford_join`.
    let needs_comma = connector == "but" || clauses.len() > 2;
    let joiner = if needs_comma { format!(", {connector} ") } else { format!(" {connector} ") };

    let body = match clauses.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{}{joiner}{last}", rest.join(", ")),
        Some((only, _)) => only.clone(),
        None => String::new(),
    };

    format!("{} is {body}.", human_label(field))
}

/// Groups `group`'s records by their raw value for `field`, returning
/// (value, sorted units) pairs — blank last, then alphabetical, mirroring
/// the reference script's own console-summary convention.
fn units_by_value<'a>(group: &'a TenantGroup, field: FieldName) -> Vec<(String, Vec<&'a str>)> {
    let mut units_by_value: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for record in &group.records {
        let raw = record.field(field).trim();
        let value = if raw.is_empty() { "(blank)".to_string() } else { raw.to_string() };
        units_by_value.entry(value).or_default().push(record.unit_number.as_str());
    }

    let mut by_value: Vec<(String, Vec<&str>)> = units_by_value.into_iter().collect();
    by_value.sort_by_key(|(value, _)| (value == "(blank)", value.clone()));

    for (_, units) in &mut by_value {
        units.sort_unstable();
    }

    by_value
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
    fn units_phrase_agrees_in_number() {
        assert_eq!(units_phrase(&["13"]), "unit 13");
        assert_eq!(units_phrase(&["13", "54"]), "units 13 and 54");
        assert_eq!(units_phrase(&["13", "54", "67"]), "units 13, 54, and 67");
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
        assert_eq!(bullets[0].1, "Email address is a@example.com for unit 101, but blank for unit 204.");
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
}
