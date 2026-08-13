//! Turns a structured finding (which categories differ, for which
//! group) into the human-facing note text. Kept as a trait specifically
//! so this is swappable: `TemplateNoteComposer` below is the v1,
//! deterministic, no-I/O implementation, matching this project's
//! principles. A future implementation could call out to an LLM for a
//! more tailored, context-aware message instead — same structured
//! input (real field values, unit numbers, tenant names already
//! computed by the rest of this crate), different composition, nothing
//! else in the pipeline needs to change.

use std::collections::HashMap;

use crate::notes::{
    note_template_for_category, relatedness_template_for_signal, NOTE_SEPARATE_TENANTS,
    NOTE_VERIFY_DIFFERS, NOTE_VERIFY_MATCHES, RELATEDNESS_TRAILER,
};
use crate::phrasing::{
    address_present_and_shared, all_addresses_present_and_distinct,
    all_emails_present_and_distinct, capitalize_first, describe_field, group_units, oxford_join,
    phone_present_and_shared, units_phrase,
};
use crate::relatedness::RelatednessSignal;
use crate::types::{FieldCategory, FieldMismatch, FieldName, TenantGroup, CATEGORY_PRIORITY};

/// One piece of relatedness evidence, as handed to `compose_relatedness_note`
/// — `member_groups` resolved to real `TenantGroup`s (not just keys) since
/// the note needs display names/units, which `relatedness.rs` deliberately
/// doesn't carry (see that module's own doc comments on why candidates only
/// carry group keys).
pub struct RelatednessEvidenceInput<'a> {
    pub signal: RelatednessSignal,
    pub shared_value: &'a str,
    pub member_groups: Vec<&'a TenantGroup>,
}

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

    /// The note for a related-tenant household — one or more pieces of
    /// evidence (each a specific, non-blank shared value under one
    /// signal, and the subset of the household's members who share
    /// it) connecting tenants with different name keys. `evidence` is
    /// always non-empty; a single entry gets the original one-signal
    /// wording verbatim, more than one composes a combined sentence.
    fn compose_relatedness_note(&self, evidence: &[RelatednessEvidenceInput]) -> String;
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

        // An email-only mismatch where the address AND phone already
        // match (both present, not merely both blank) is corroborating
        // evidence this is one person who typo'd their email, not two
        // tenants — name that outright rather than leaving the reader
        // to notice the matching address/phone on their own. Distinct
        // from the separate-tenants branch above: that one requires
        // Address to *differ*, this one requires it to already match.
        if differing.len() == 1
            && differing[0].category == FieldCategory::Email
            && address_present_and_shared(group)
            && phone_present_and_shared(group)
        {
            return format!(
                "{lead} {detail} The matching address and phone suggest this is one person \
                 with a mistyped email, not two separate tenants."
            );
        }

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

    fn compose_relatedness_note(&self, evidence: &[RelatednessEvidenceInput]) -> String {
        // The common case: one household, one piece of evidence — use
        // the original single-signal wording verbatim, unchanged from
        // before this module supported multi-evidence households.
        if let [only] = evidence {
            let names = relatedness_names_phrase(&only.member_groups);
            return relatedness_template_for_signal(only.signal)
                .replace("{names}", &names)
                .replace("{value}", only.shared_value);
        }

        // More than one piece of evidence: group by exactly which
        // members it connects first, so multiple signals/values shared
        // by the *same* pair combine into one clause ("share the same
        // phone numbers (X and Y), email address (Z), and alternate
        // contact (W)") instead of repeating "A and B" once per signal
        // — the case that motivated this in the first place (two
        // tenants identical on every field emitting one clause, not
        // five).
        let mut subset_order: Vec<Vec<String>> = Vec::new();
        let mut by_subset: HashMap<Vec<String>, Vec<&RelatednessEvidenceInput>> = HashMap::new();
        for item in evidence {
            let mut keys: Vec<String> = item.member_groups.iter().map(|g| g.key.clone()).collect();
            keys.sort();
            if !by_subset.contains_key(&keys) {
                subset_order.push(keys.clone());
            }
            by_subset.entry(keys).or_default().push(item);
        }

        let clauses: Vec<String> = subset_order
            .iter()
            .map(|keys| {
                let items = &by_subset[keys];
                let names = relatedness_names_phrase(&items[0].member_groups);

                let noun_phrases: Vec<String> = [
                    RelatednessSignal::SharedPhone,
                    RelatednessSignal::SharedEmail,
                    RelatednessSignal::SharedAlternateContact,
                    RelatednessSignal::SharedHomeAddress,
                ]
                .into_iter()
                .filter_map(|signal| {
                    let values: Vec<&str> = items
                        .iter()
                        .filter(|item| item.signal == signal)
                        .map(|item| item.shared_value)
                        .collect();
                    if values.is_empty() {
                        None
                    } else {
                        Some(format!(
                            "{} ({})",
                            signal_noun(signal, values.len()),
                            oxford_join(&values)
                        ))
                    }
                })
                .collect();
                let noun_phrase_refs: Vec<&str> = noun_phrases.iter().map(String::as_str).collect();

                format!("{names} share the same {}", oxford_join(&noun_phrase_refs))
            })
            .collect();

        format!(
            "{}{RELATEDNESS_TRAILER}",
            capitalize_first(&clauses.join("; "))
        )
    }
}

fn relatedness_names_phrase(groups: &[&TenantGroup]) -> String {
    groups
        .iter()
        .map(|g| {
            format!(
                "{} ({})",
                g.records[0].display_name(),
                units_phrase(&group_units(g))
            )
        })
        .collect::<Vec<_>>()
        .join(" and ")
}

/// Singular/plural noun for a relatedness signal, e.g. "phone number"
/// vs. "phone numbers" when the same pair shares two different phone
/// numbers (both their own and their alternate contact's, say).
fn signal_noun(signal: RelatednessSignal, count: usize) -> &'static str {
    match (signal, count > 1) {
        (RelatednessSignal::SharedPhone, false) => "phone number",
        (RelatednessSignal::SharedPhone, true) => "phone numbers",
        (RelatednessSignal::SharedEmail, false) => "email address",
        (RelatednessSignal::SharedEmail, true) => "email addresses",
        (RelatednessSignal::SharedAlternateContact, false) => "alternate contact",
        (RelatednessSignal::SharedAlternateContact, true) => "alternate contacts",
        (RelatednessSignal::SharedHomeAddress, false) => "home address",
        (RelatednessSignal::SharedHomeAddress, true) => "home addresses",
    }
}

#[cfg(test)]
#[path = "note_composer_tests.rs"]
mod tests;
