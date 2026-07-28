//! Generic English-phrasing helpers used when composing note text —
//! split out of `note_composer.rs` since none of these depend on the
//! `NoteComposer` trait or its implementations; they're plain string
//! formatting over `TenantGroup`/`FieldName` data.

use std::collections::HashSet;

use crate::normalization::normalize_value;
use crate::types::{kind_for, FieldName, TenantGroup};

/// This group's unit numbers, sorted. The base list `units_phrase` turns
/// into a properly-worded phrase — kept separate so callers that need
/// the raw list (e.g. the API layer building a `units: Vec<String>`
/// field for the frontend) don't have to parse a phrase back apart.
pub fn group_units(group: &TenantGroup) -> Vec<&str> {
    let mut units: Vec<&str> = group
        .records
        .iter()
        .map(|r| r.unit_number.as_str())
        .collect();
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

pub(crate) fn capitalize_first(s: &str) -> String {
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
pub(crate) fn describe_field(group: &TenantGroup, field: FieldName) -> String {
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
    let joiner = if needs_comma {
        format!(", {connector} ")
    } else {
        format!(" {connector} ")
    };

    let body = match clauses.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{}{joiner}{last}", rest.join(", ")),
        Some((only, _)) => only.clone(),
        None => String::new(),
    };

    format!("{} is {body}.", human_label(field))
}

/// Groups `group`'s records by the same `(is_blank, normalized value)`
/// key `comparison::field_matches_across` uses to decide a mismatch —
/// not the raw value — so two records that count as "the same" there
/// (e.g. two phone numbers differing only in formatting) land in one
/// clause here too, instead of the note claiming more distinct values
/// disagree than actually do. Reports the *first* raw value seen per key
/// (not the normalized form) so the note still reads like real data.
/// Returns (value, sorted units) pairs — blank last, then alphabetical by
/// that display value, mirroring the reference script's own
/// console-summary convention.
fn units_by_value(group: &TenantGroup, field: FieldName) -> Vec<(String, Vec<&str>)> {
    let kind = kind_for(field);

    let mut by_key: std::collections::BTreeMap<(bool, String), (String, Vec<&str>)> =
        std::collections::BTreeMap::new();

    for record in &group.records {
        let raw = record.field(field).trim();
        let blank = raw.is_empty();
        let display = if blank {
            "(blank)".to_string()
        } else {
            raw.to_string()
        };
        let key = (blank, normalize_value(kind, raw));

        let entry = by_key.entry(key).or_insert_with(|| (display, Vec::new()));
        entry.1.push(record.unit_number.as_str());
    }

    let mut by_value: Vec<(String, Vec<&str>)> = by_key.into_values().collect();
    by_value.sort_by_key(|(value, _)| (value == "(blank)", value.clone()));

    for (_, units) in &mut by_value {
        units.sort_unstable();
    }

    by_value
}

/// True if every record's email is present (non-blank) and distinct —
/// the "these might just be separate tenants sharing a name" signal,
/// as opposed to a genuine mismatch to fix.
pub(crate) fn all_emails_present_and_distinct(group: &TenantGroup) -> bool {
    let emails: Vec<String> = group
        .records
        .iter()
        .map(|r| r.email.trim().to_lowercase())
        .collect();
    if emails.iter().any(|e| e.is_empty()) {
        return false;
    }
    let unique: HashSet<&String> = emails.iter().collect();
    unique.len() == emails.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TenantRecord;

    #[test]
    fn units_phrase_agrees_in_number() {
        assert_eq!(units_phrase(&["13"]), "unit 13");
        assert_eq!(units_phrase(&["13", "54"]), "units 13 and 54");
        assert_eq!(units_phrase(&["13", "54", "67"]), "units 13, 54, and 67");
    }

    /// Regression test: two differently-formatted but equal phone numbers
    /// must merge into one clause, not appear as two separate "distinct"
    /// values in the generated sentence.
    #[test]
    fn units_by_value_merges_differently_formatted_but_equal_values() {
        let group = TenantGroup {
            key: "test".to_string(),
            records: vec![
                TenantRecord {
                    unit_number: "10".into(),
                    phone_number: "(555) 123-4567".into(),
                    ..Default::default()
                },
                TenantRecord {
                    unit_number: "20".into(),
                    phone_number: "555-123-4567".into(),
                    ..Default::default()
                },
                TenantRecord {
                    unit_number: "30".into(),
                    phone_number: "".into(),
                    ..Default::default()
                },
            ],
        };

        let by_value = units_by_value(&group, FieldName::PhoneNumber);

        assert_eq!(
            by_value.len(),
            2,
            "the two differently-formatted-but-equal phone numbers should merge into \
             one clause, leaving just that value plus \"(blank)\": {:?}",
            by_value
        );
    }
}
