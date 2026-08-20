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
//!
//! The transitive-closure union-find that turns per-signal shared-value
//! clusters into households lives in `household` -- a genuinely separate
//! concern from gathering the per-signal evidence and composing each
//! household's note, which stay here.

mod household;

use std::collections::HashMap;

use serde::Serialize;

use household::{group_into_households, RawEvidence};

use crate::normalization::{is_empty, is_placeholder, normalize_value};
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

/// A normalized phone value with fewer digits than a real US phone
/// number can have is a truncated fragment (an area-code stub, a
/// partial paste), not a real shared identity — e.g. `"978"` sitting
/// in `AlternateContactPhoneNumber` on two otherwise-unrelated
/// records. `v` is expected already digit-only (post
/// `normalize_value(FieldKind::Phone, _)`), so this is a plain length
/// check, not a second normalization pass.
const MIN_PHONE_DIGITS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RelatednessSignal {
    SharedPhone,
    SharedEmail,
    SharedAlternateContact,
    SharedHomeAddress,
}

/// One piece of evidence within a household: `group_keys` (a subset of
/// the household's full member list, always at least 2, at most
/// `MAX_CLUSTER_SIZE`) share `shared_value` under `signal`. A household
/// with evidence from more than one signal, or more than one value
/// under the same signal, carries one of these per distinct
/// (signal, value) pair — not one per pair of tenants, since three
/// tenants sharing one phone number is one piece of evidence, not
/// three.
#[derive(Debug, Clone, Serialize)]
pub struct RelatedTenantEvidence {
    pub signal: RelatednessSignal,
    pub shared_value: String,
    pub group_keys: Vec<String>,
}

/// A household: the full transitive union of every tenant connected to
/// every other by *any* shared-value evidence, despite having
/// different name keys. `group_keys` (sorted, at least 2, at most
/// `MAX_HOUSEHOLD_SIZE`) is the union across every entry in `evidence`
/// — not every member necessarily shares every piece of evidence with
/// every other member directly, only transitively through the chain.
/// `note` is one composed account of every piece of evidence, not one
/// note per signal — see `note_composer::compose_relatedness_note`.
#[derive(Debug, Clone, Serialize)]
pub struct RelatedTenantCandidate {
    pub group_keys: Vec<String>,
    pub evidence: Vec<RelatedTenantEvidence>,
    pub note: String,
}

/// Runs all four signals over `groups` (every distinct tenant, not
/// just multi-unit ones — a relationship can exist between two
/// single-unit tenants), then merges every resulting (signal, value)
/// cluster into households by transitive closure (see `household`):
/// two clusters that share even one tenant belong to the same
/// household, and every piece of evidence connecting any member is
/// kept, not just the evidence connecting the specific pair a reader
/// might expect.
pub fn find_related_tenant_candidates(
    groups: &[TenantGroup],
    composer: &dyn NoteComposer,
) -> Vec<RelatedTenantCandidate> {
    let mut raw = Vec::new();
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
            raw.push(RawEvidence {
                signal,
                value,
                keys,
            });
        }
    }

    let mut candidates = Vec::new();
    for (member_set, mut evidence_list) in group_into_households(raw) {
        let mut group_keys: Vec<String> = member_set.into_iter().collect();
        group_keys.sort();

        evidence_list.sort_by(|a, b| {
            signal_priority(a.signal)
                .cmp(&signal_priority(b.signal))
                .then(a.value.cmp(&b.value))
        });

        let member_groups: Vec<Vec<&TenantGroup>> = evidence_list
            .iter()
            .map(|e| {
                e.keys
                    .iter()
                    .filter_map(|key| groups.iter().find(|g| &g.key == key))
                    .collect()
            })
            .collect();

        let evidence_input: Vec<crate::note_composer::RelatednessEvidenceInput> = evidence_list
            .iter()
            .zip(&member_groups)
            .map(
                |(e, member_group)| crate::note_composer::RelatednessEvidenceInput {
                    signal: e.signal,
                    shared_value: &e.value,
                    member_groups: member_group.clone(),
                },
            )
            .collect();

        let note = composer.compose_relatedness_note(&evidence_input);

        let evidence: Vec<RelatedTenantEvidence> = evidence_list
            .into_iter()
            .map(|e| RelatedTenantEvidence {
                signal: e.signal,
                shared_value: e.value,
                group_keys: e.keys,
            })
            .collect();

        candidates.push(RelatedTenantCandidate {
            group_keys,
            evidence,
            note,
        });
    }

    candidates.sort_by(|a, b| {
        b.group_keys
            .len()
            .cmp(&a.group_keys.len())
            .then(a.group_keys.cmp(&b.group_keys))
    });
    candidates
}

/// Stable display order for a household's evidence list — same
/// priority family as `CATEGORY_PRIORITY` elsewhere in this crate
/// (phone/email first), so a reader sees the strongest-feeling
/// evidence first regardless of which signal happened to be found
/// first during the scan.
fn signal_priority(signal: RelatednessSignal) -> u8 {
    match signal {
        RelatednessSignal::SharedPhone => 0,
        RelatednessSignal::SharedEmail => 1,
        RelatednessSignal::SharedAlternateContact => 2,
        RelatednessSignal::SharedHomeAddress => 3,
    }
}

/// Maps each shared, normalized value to every distinct tenant
/// (group key) it appears on, for one signal. A value appearing on
/// only one tenant is not a cluster (nothing shared); one appearing on
/// more than `MAX_CLUSTER_SIZE` is filtered by the caller.
fn find_clusters(
    groups: &[TenantGroup],
    signal: RelatednessSignal,
) -> HashMap<String, Vec<String>> {
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
/// from. Normalized as `FieldKind::Phone` (digits only) so two
/// differently-formatted renderings of the same number ("(831)
/// 555-1234" vs. "8315551234") are still recognized as the same shared
/// value, matching how `comparison.rs` now normalizes these same
/// fields via `FIELD_SPECS`.
fn phone_values(group: &TenantGroup) -> Vec<String> {
    group
        .records
        .iter()
        .flat_map(|r| [r.phone_number.as_str(), r.alt_contact_phone_number.as_str()])
        .filter(|v| !is_empty(v) && !is_placeholder(v))
        .map(|v| normalize_value(FieldKind::Phone, v))
        .filter(|v| v.len() >= MIN_PHONE_DIGITS)
        .collect()
}

fn email_values(group: &TenantGroup) -> Vec<String> {
    group
        .records
        .iter()
        .flat_map(|r| [r.email.as_str(), r.alt_contact_email.as_str()])
        .filter(|v| !is_empty(v) && !is_placeholder(v))
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
            let name = format!(
                "{} {}",
                r.alt_contact_first_name.trim(),
                r.alt_contact_last_name.trim()
            );
            let name = name.trim();
            if name.is_empty() || is_placeholder(name) {
                None
            } else {
                Some(normalize_value(FieldKind::Plain, name))
            }
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

/// `pub(crate)` so `phrasing::all_addresses_present_and_distinct` (and
/// `address_present_and_shared`) can reuse the exact same "what counts
/// as the same address" rule, rather than a second, possibly-drifting
/// definition living in two places.
pub(crate) fn full_address(
    street1: &str,
    street2: &str,
    city: &str,
    state: &str,
    postal: &str,
) -> Option<String> {
    if is_empty(street1) || is_placeholder(street1) {
        return None;
    }
    // Every field keeps its position in the join, blank or not -- dropping
    // blank fields before joining (the previous behavior) let two
    // addresses whose data lands in different columns (e.g. one export's
    // city sitting in `street2`, another's in `city` -- a real
    // vendor-format inconsistency this project already documents
    // elsewhere) collapse to the identical joined string and register as
    // a false "shared address."
    let joined = [street1, street2, city, state, postal]
        .iter()
        .map(|v| normalize_value(FieldKind::Address, v))
        .collect::<Vec<_>>()
        .join(", ");
    Some(joined)
}

#[cfg(test)]
#[path = "../relatedness_tests.rs"]
mod tests;
