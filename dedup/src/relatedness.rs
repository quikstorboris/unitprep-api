//! Pass over *different-name-key* tenant groups looking for a shared,
//! specific, non-blank identifying detail — a phone number, an email
//! address, an alternate-contact identity, or a full home address that
//! appears on two or more tenants who don't share a name at all. This
//! catches a real relationship pattern (a business and its owner,
//! family members, a subdivided unit) that neither exact-name grouping
//! nor typo-variant similarity could ever find, since both of those
//! hinge entirely on name similarity.
//!
//! Bare unit-number adjacency (e.g. 81F/81G/81H) was considered and
//! deliberately rejected as a trigger on its own — it's real-world
//! signal, but far too weak alone (see project history). This module
//! only fires on a specific shared value, never on adjacency by
//! itself; adjacency, if present, is mentioned as supporting context in
//! the note, never a precondition.
//!
//! Always advisory — same policy as typo-variant candidates (see
//! `report`'s crate-level docs): every candidate is surfaced for human
//! review, nothing here ever implies or merges shared identity.

use std::collections::HashMap;

use serde::Serialize;

use crate::normalization::{is_empty, normalize_value};
use crate::note_composer::NoteComposer;
use crate::types::{FieldKind, TenantGroup};

/// Caps how many distinct tenants a single shared value can connect
/// before it's excluded entirely. A value connecting a *small* number
/// of tenants is real signal; a value connecting many (a shared office
/// phone number, a generic mailing address reused facility-wide) is
/// far more likely a data artifact than an actual relationship.
/// Deliberately small and conservative — revisit only if real data
/// shows a genuine relationship this size being missed, not
/// speculatively.
const MAX_CLUSTER_SIZE: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RelatednessSignal {
    SharedPhone,
    SharedEmail,
    SharedAlternateContact,
    SharedHomeAddress,
}

/// Two or more tenants (by group key) who share `shared_value` under
/// `signal`, despite having different name keys. `group_keys` is
/// sorted and always has at least 2, at most `MAX_CLUSTER_SIZE` entries
/// — anything outside that range is filtered out before this type is
/// ever constructed.
#[derive(Debug, Clone, Serialize)]
pub struct RelatedTenantCandidate {
    pub group_keys: Vec<String>,
    pub signal: RelatednessSignal,
    pub shared_value: String,
    pub note: String,
}

/// Runs all four signals over `groups` (every distinct tenant, not
/// just multi-unit ones — a relationship can exist between two
/// single-unit tenants) and returns every surfaced candidate, most
/// tenants connected first.
pub fn find_related_tenant_candidates(
    groups: &[TenantGroup],
    composer: &dyn NoteComposer,
) -> Vec<RelatedTenantCandidate> {
    let mut candidates = Vec::new();

    for signal in [
        RelatednessSignal::SharedPhone,
        RelatednessSignal::SharedEmail,
        RelatednessSignal::SharedAlternateContact,
        RelatednessSignal::SharedHomeAddress,
    ] {
        let clusters = find_clusters(groups, signal);
        for (value, mut keys) in clusters {
            if keys.len() < 2 || keys.len() > MAX_CLUSTER_SIZE {
                continue;
            }
            keys.sort();

            let member_groups: Vec<&TenantGroup> =
                keys.iter().filter_map(|key| groups.iter().find(|g| &g.key == key)).collect();

            let note = composer.compose_relatedness_note(&member_groups, signal, &value);

            candidates.push(RelatedTenantCandidate { group_keys: keys, signal, shared_value: value, note });
        }
    }

    candidates.sort_by(|a, b| b.group_keys.len().cmp(&a.group_keys.len()).then(a.group_keys.cmp(&b.group_keys)));
    candidates
}

/// Maps each shared, normalized value to every distinct tenant
/// (group key) it appears on, for one signal. A value appearing on
/// only one tenant is not a cluster (nothing shared); one appearing on
/// more than `MAX_CLUSTER_SIZE` is filtered by the caller.
fn find_clusters(groups: &[TenantGroup], signal: RelatednessSignal) -> HashMap<String, Vec<String>> {
    let mut clusters: HashMap<String, Vec<String>> = HashMap::new();

    for group in groups {
        let mut seen_in_this_group = std::collections::HashSet::new();
        for value in values_for_signal(group, signal) {
            if value.is_empty() || !seen_in_this_group.insert(value.clone()) {
                continue;
            }
            let keys = clusters.entry(value).or_default();
            if !keys.contains(&group.key) {
                keys.push(group.key.clone());
            }
        }
    }

    clusters
}

fn values_for_signal(group: &TenantGroup, signal: RelatednessSignal) -> Vec<String> {
    match signal {
        RelatednessSignal::SharedPhone => phone_values(group),
        RelatednessSignal::SharedEmail => email_values(group),
        RelatednessSignal::SharedAlternateContact => alt_contact_identities(group),
        RelatednessSignal::SharedHomeAddress => address_values(group),
    }
}

/// Both the primary and alternate-contact phone number count as "this
/// tenant's known phone numbers" — the signal is "this literal number
/// connects two tenants somehow," not which specific field it came
/// from.
fn phone_values(group: &TenantGroup) -> Vec<String> {
    group
        .records
        .iter()
        .flat_map(|r| [r.phone_number.as_str(), r.alt_contact_phone_number.as_str()])
        .filter(|v| !is_empty(v))
        .map(|v| normalize_value(FieldKind::Plain, v))
        .collect()
}

fn email_values(group: &TenantGroup) -> Vec<String> {
    group
        .records
        .iter()
        .flat_map(|r| [r.email.as_str(), r.alt_contact_email.as_str()])
        .filter(|v| !is_empty(v))
        .map(|v| normalize_value(FieldKind::Plain, v))
        .collect()
}

/// Unlike phone/email, this is about the alternate contact's *name*,
/// not their phone/email (those are already covered by the two signals
/// above) — two different primary tenants listing the same person by
/// name as their alternate contact is its own distinct piece of
/// evidence, even if that person's own phone/email is blank or differs
/// between the two listings.
fn alt_contact_identities(group: &TenantGroup) -> Vec<String> {
    group
        .records
        .iter()
        .filter_map(|r| {
            let name = format!("{} {}", r.alt_contact_first_name.trim(), r.alt_contact_last_name.trim());
            let name = name.trim();
            if name.is_empty() { None } else { Some(normalize_value(FieldKind::Plain, name)) }
        })
        .collect()
}

/// Both the primary and alternate-contact address count. A blank
/// street address is never treated as a real address to compare —
/// otherwise two tenants who both merely happen to share a city (with
/// no street on file for either) would falsely "share an address,"
/// which is far too loose a bar.
fn address_values(group: &TenantGroup) -> Vec<String> {
    group
        .records
        .iter()
        .flat_map(|r| {
            [
                full_address(
                    &r.address_street1,
                    &r.address_street2,
                    &r.address_city,
                    &r.address_state,
                    &r.address_postal_code,
                ),
                full_address(
                    &r.alt_contact_address_street1,
                    &r.alt_contact_address_street2,
                    &r.alt_contact_address_city,
                    &r.alt_contact_address_state,
                    &r.alt_contact_address_postal_code,
                ),
            ]
        })
        .flatten()
        .collect()
}

fn full_address(street1: &str, street2: &str, city: &str, state: &str, postal: &str) -> Option<String> {
    if is_empty(street1) {
        return None;
    }
    let joined = [street1, street2, city, state, postal]
        .iter()
        .map(|v| normalize_value(FieldKind::Address, v))
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    Some(joined)
}

#[cfg(test)]
#[path = "relatedness_tests.rs"]
mod tests;
