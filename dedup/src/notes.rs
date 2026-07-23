//! Human-facing note text: templates only, no composition logic — see
//! `note_composer` for how a template gets picked and filled in.
//! Wording follows the reference script's `NOTES`, extended with a
//! `{units}` placeholder so a composed note can say which units are
//! affected instead of a flat "across all units".

use crate::relatedness::RelatednessSignal;
use crate::types::FieldCategory;

// Every `{units}`/`{units_a}`/`{units_b}` placeholder below is
// substituted with the *whole* phrase (the word "unit"/"units" already
// agreeing in number, plus the actual unit numbers — e.g. "unit 13" or
// "units 54, 67, and 77"), not just the bare numbers — see
// `note_composer::units_phrase`. Templates don't hardcode the word
// "units" themselves so a single-unit case doesn't read as "unit 13"
// with a leftover plural "units" already baked into the template.
pub const NOTE_PHONE: &str = "Please update the phone number to match across {units}.";
pub const NOTE_EMAIL: &str = "Please update the email address to match across {units}.";
pub const NOTE_ADDRESS: &str = "Please update the address to match across {units}.";
pub const NOTE_ALT_CONTACT: &str =
    "Please update the alternate contact info to match across {units}.";
pub const NOTE_COMPANY: &str = "Company name differs across {units} — check whether one \
    value is a stray note (e.g. a deposit amount or phone number) rather than an actual company \
    name, and update/clear as needed.";
pub const NOTE_NAME: &str = "Change the name across {units} if these should be two separate \
    tenants.";

pub const NOTE_SEPARATE_TENANTS: &str = "{units} share a name but have different email \
    addresses — these may be separate tenants; if so, try to obtain a unique email address for \
    each.";

/// Shown for a typo-variant candidate whose contact info differs
/// between the two tenants.
pub const NOTE_VERIFY_DIFFERS: &str = "{name_a} ({units_a}) and {name_b} ({units_b}) \
    have very similar names but differing contact info — verify whether this is the same tenant \
    before consolidating.";

/// Shown for a typo-variant candidate whose contact info already
/// matches between the two tenants.
pub const NOTE_VERIFY_MATCHES: &str = "{name_a} ({units_a}) and {name_b} ({units_b}) \
    may be the same tenant — all other contact info matches; verify and correct the name if so.";

/// The base template for a category, before placeholder substitution.
pub fn note_template_for_category(category: FieldCategory) -> &'static str {
    match category {
        FieldCategory::Phone => NOTE_PHONE,
        FieldCategory::Email => NOTE_EMAIL,
        FieldCategory::Address => NOTE_ADDRESS,
        FieldCategory::AltContact => NOTE_ALT_CONTACT,
        FieldCategory::Company => NOTE_COMPANY,
        FieldCategory::Name => NOTE_NAME,
    }
}

/// Shown for a related-tenant candidate — different name keys sharing a
/// specific, non-blank value. Wording is deliberately the same shape
/// across all four signals (only the noun changes) so the four read as
/// one consistent family of finding, not four unrelated ones.
pub const NOTE_SHARED_PHONE: &str = "{names} share the same phone number ({value}) despite having \
    different names — worth checking whether these are related tenants.";
pub const NOTE_SHARED_EMAIL: &str = "{names} share the same email address ({value}) despite having \
    different names — worth checking whether these are related tenants.";
pub const NOTE_SHARED_ALT_CONTACT: &str = "{names} both list the same alternate contact ({value}) \
    despite having different names — worth checking whether these are related tenants.";
pub const NOTE_SHARED_ADDRESS: &str = "{names} share the same home address ({value}) despite having \
    different names — worth checking whether these are related tenants.";

/// The base template for a relatedness signal, before placeholder
/// substitution.
pub fn relatedness_template_for_signal(signal: RelatednessSignal) -> &'static str {
    match signal {
        RelatednessSignal::SharedPhone => NOTE_SHARED_PHONE,
        RelatednessSignal::SharedEmail => NOTE_SHARED_EMAIL,
        RelatednessSignal::SharedAlternateContact => NOTE_SHARED_ALT_CONTACT,
        RelatednessSignal::SharedHomeAddress => NOTE_SHARED_ADDRESS,
    }
}
