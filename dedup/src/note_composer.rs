//! Turns a structured finding (which categories differ, for which
//! group) into the human-facing note text. Kept as a trait specifically
//! so this is swappable: `TemplateNoteComposer` below is the v1,
//! deterministic, no-I/O implementation, matching this project's
//! principles. A future implementation could call out to an LLM for a
//! more tailored, context-aware message instead — same structured
//! input (real field values, unit numbers, tenant names already
//! computed by the rest of this crate), different composition, nothing
//! else in the pipeline needs to change.

use crate::notes::{
    note_template_for_category, relatedness_template_for_signal, NOTE_SEPARATE_TENANTS,
    NOTE_VERIFY_DIFFERS, NOTE_VERIFY_MATCHES,
};
use crate::phrasing::{
    all_addresses_present_and_distinct, all_emails_present_and_distinct, capitalize_first,
    describe_field, group_units, units_phrase,
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

        // "Separate tenants sharing a name" only makes sense when Email
        // AND Address both differ (and each is non-blank/distinct across
        // the group) — two people who happen to share a name plausibly
        // don't also share a home address. The previous condition
        // (Email as the *sole* differing category) could never actually
        // reach that scenario: `find_differing_categories` only lists a
        // category when something in it differs, so "Email is the only
        // entry" structurally meant Address already matched — i.e. this
        // branch fired exactly for the same-person-typo'd-their-email
        // case, the opposite of what it claimed. Confirmed against real
        // production data (two units, one tenant, a one-character email
        // typo, identical address/phone) getting told they "may be
        // separate tenants."
        if differing.len() == 2
            && differing.iter().any(|m| m.category == FieldCategory::Email)
            && differing
                .iter()
                .any(|m| m.category == FieldCategory::Address)
            && all_emails_present_and_distinct(group)
            && all_addresses_present_and_distinct(group)
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
            .map(|field_mismatch| {
                (
                    field_mismatch.field,
                    describe_field(group, field_mismatch.field),
                )
            })
            .collect()
    }

    fn compose_variant_note(
        &self,
        group_a: &TenantGroup,
        group_b: &TenantGroup,
        contact_info_matches: bool,
    ) -> String {
        let template = if contact_info_matches {
            NOTE_VERIFY_MATCHES
        } else {
            NOTE_VERIFY_DIFFERS
        };
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
            .map(|g| {
                format!(
                    "{} ({})",
                    g.records[0].display_name(),
                    units_phrase(&group_units(g))
                )
            })
            .collect::<Vec<_>>()
            .join(" and ");

        relatedness_template_for_signal(signal)
            .replace("{names}", &names)
            .replace("{value}", shared_value)
    }
}

#[cfg(test)]
#[path = "note_composer_tests.rs"]
mod tests;
