//! Union-find grouping of per-signal shared-value evidence into
//! households -- the transitive-closure step behind
//! `find_related_tenant_candidates`: two clusters that share even one
//! tenant belong to the same household, and every piece of evidence
//! connecting any member is kept, not just the evidence connecting the
//! specific pair a reader might expect. This is what turns e.g. five
//! separate "Diana and Donald share X" rows (one per shared field) into
//! one row naming Diana and Donald once with all five pieces of evidence
//! listed, and what connects a three-tenant chain (A shares a phone with
//! B, B shares an email with C, A and C share nothing directly) into one
//! household instead of two disjoint pairs.

use std::collections::{HashMap, HashSet};

use super::RelatednessSignal;

/// Caps how large a *household* (the transitive union of every tenant
/// connected via any signal, not any single value) can grow before
/// it's excluded entirely. Deliberately more generous than
/// `MAX_CLUSTER_SIZE`, since a household's evidence has already been
/// individually filtered — this guards only against a pathological
/// chain (A-B via one value, B-C via an unrelated value, C-D via a
/// third, none of them individually too-common) accreting into one
/// implausibly large "family," not against normal household size.
pub(super) const MAX_HOUSEHOLD_SIZE: usize = 8;

/// One piece of evidence not yet assigned to a household: `signal` and
/// `value` name what was shared, `keys` are the (at least 2, at most
/// `MAX_CLUSTER_SIZE`) group keys it was found on.
pub(super) struct RawEvidence {
    pub(super) signal: RelatednessSignal,
    pub(super) value: String,
    pub(super) keys: Vec<String>,
}

// Union-find over group keys: every key in one piece of evidence joins
// the same household. `union`ing each adjacent pair in a slice
// transitively joins the whole slice, since `find` follows parent chains
// to their root regardless of how many hops deep.
fn find(parent: &mut HashMap<String, String>, key: &str) -> String {
    parent
        .entry(key.to_string())
        .or_insert_with(|| key.to_string());
    let mut root = key.to_string();
    while parent[&root] != root {
        root = parent[&root].clone();
    }
    // Path compression: point every visited node straight at the
    // root, so repeated lookups on a long chain stay cheap.
    let mut node = key.to_string();
    while parent[&node] != root {
        let next = parent[&node].clone();
        parent.insert(node, root.clone());
        node = next;
    }
    root
}

fn union(parent: &mut HashMap<String, String>, a: &str, b: &str) {
    let root_a = find(parent, a);
    let root_b = find(parent, b);
    if root_a != root_b {
        parent.insert(root_a, root_b);
    }
}

/// Unions every piece of evidence's keys via union-find, buckets `raw`
/// by household root, and drops any household exceeding
/// `MAX_HOUSEHOLD_SIZE`. Returns one `(member_keys, evidence)` pair per
/// surviving household; the caller (`find_related_tenant_candidates`)
/// sorts `member_keys` for display and re-sorts `evidence` by display
/// priority -- this function's job ends at "which tenants and which
/// evidence belong together," not how either reads on screen.
pub(super) fn group_into_households(
    raw: Vec<RawEvidence>,
) -> Vec<(HashSet<String>, Vec<RawEvidence>)> {
    let mut parent: HashMap<String, String> = HashMap::new();

    for evidence in &raw {
        for pair in evidence.keys.windows(2) {
            union(&mut parent, &pair[0], &pair[1]);
        }
    }

    // Bucket every piece of evidence under its household's root key.
    let mut households: HashMap<String, (HashSet<String>, Vec<RawEvidence>)> = HashMap::new();
    for evidence in raw {
        let root = find(&mut parent, &evidence.keys[0]);
        let entry = households.entry(root).or_default();
        entry.0.extend(evidence.keys.iter().cloned());
        entry.1.push(evidence);
    }

    households
        .into_values()
        .filter(|(member_set, _)| member_set.len() <= MAX_HOUSEHOLD_SIZE)
        .collect()
}
