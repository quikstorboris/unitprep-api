//! Pass 3: turn a group's differing categories into a single
//! human-facing correction note. Ported from the reference script's
//! `NOTES` / `correction_note`, including the "all emails distinct"
//! special case (2026-07-14 revision moved `CompanyName` into its own
//! `company` category with its own note wording).

use std::collections::HashSet;

use crate::types::{FieldCategory, FieldMismatch, TenantRecord, CATEGORY_PRIORITY};

pub const NOTE_PHONE: &str = "Please update phone numbers to match";
pub const NOTE_EMAIL: &str = "Update email address to match across all units";
pub const NOTE_ADDRESS: &str = "Update address to match across all units";
pub const NOTE_ALT_CONTACT: &str = "Update alternate contact to match across all units";
pub const NOTE_COMPANY: &str = "Company Name differs between units — check whether one value is a \
    stray note (e.g. a deposit amount or phone number) rather than an \
    actual company name, and update/clear as needed";
pub const NOTE_NAME: &str = "Change the name if these should be two separate tenants";

pub const NOTE_SEPARATE_TENANTS: &str = "These may be separate tenants sharing the same name — \
    if so, try to obtain a unique email address for each";

/// Shown for a typo-variant candidate whose contact info differs
/// between the two tenants.
pub const NOTE_VERIFY_DIFFERS: &str = "Verify if same tenant — names are nearly identical but \
    contact info differs between these units; consolidate if same person";

/// Shown for a typo-variant candidate whose contact info already
/// matches between the two tenants.
pub const NOTE_VERIFY_MATCHES: &str = "Verify if same tenant — all contact info matches despite \
    different names; correct the name to match if same person";

fn note_for_category(category: FieldCategory) -> &'static str {
    match category {
        FieldCategory::Phone => NOTE_PHONE,
        FieldCategory::Email => NOTE_EMAIL,
        FieldCategory::Address => NOTE_ADDRESS,
        FieldCategory::AltContact => NOTE_ALT_CONTACT,
        FieldCategory::Company => NOTE_COMPANY,
        FieldCategory::Name => NOTE_NAME,
    }
}

/// Picks the correction note for a flagged group: the "all emails
/// distinct" special case if `Email` is the *only* differing category,
/// otherwise the highest-priority differing category's note.
pub fn correction_note(group: &[TenantRecord], differing: &[FieldMismatch]) -> String {
    if differing.len() == 1 && differing[0].category == FieldCategory::Email {
        if all_emails_present_and_distinct(group) {
            return NOTE_SEPARATE_TENANTS.to_string();
        }
    }
    let differing_categories: Vec<FieldCategory> =
        differing.iter().map(|m| m.category).collect();
    for category in CATEGORY_PRIORITY {
        if differing_categories.contains(&category) {
            return note_for_category(category).to_string();
        }
    }
    String::new()
}

fn all_emails_present_and_distinct(group: &[TenantRecord]) -> bool {
    let emails: Vec<String> = group
        .iter()
        .map(|r| r.email.trim().to_lowercase())
        .collect();
    if emails.iter().any(|e| e.is_empty()) {
        return false;
    }
    let unique: HashSet<&String> = emails.iter().collect();
    unique.len() == emails.len()
}
