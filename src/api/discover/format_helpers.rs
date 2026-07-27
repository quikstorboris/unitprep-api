//! Header-shape and vendor-classification helpers shared across
//! discovery and the unit-file confirm/map flow. Column presence is
//! decided through `CsvDocument::header_index` — the single
//! normalization rule every lookup in the system shares (see its own
//! doc comment) — rather than each caller building its own normalized
//! header list. That's deliberate: this crate previously had its own
//! ad hoc `normalize()` that stripped spaces/underscores while
//! `header_index` only lowercased, so a header like "Unit_Group" could
//! pass one check and then silently fail every subsequent lookup.

use unitprep_core::csv_document::CsvDocument;

use crate::application::unit_group_session::Session;

/// Which of `discovery.selected_unit_file_names` a confirm/map action
/// applies to -- the same deterministic "first pending, sorted" notion
/// `compute_discovery` uses for `current_unit_file_name`, factored out so
/// `/unit-file/resolve-format` (see `resolve_unit_format.rs`) agrees with
/// it without duplicating the sort/filter.
pub(crate) fn current_unit_file_to_resolve(
    session: &Session,
) -> Option<String> {
    session
        .data
        .discovery
        .as_ref()?
        .current_unit_file_name
        .clone()
}

/// Order-insensitive header identity for comparing two documents' shapes
/// -- shared by `find_header_mismatches` below and
/// `resolve_unit_format`'s bulk-confirm logic, which needs the exact same
/// notion of "same shape" to decide which confirmed files a single
/// vendor confirmation covers.
pub(crate) fn normalized_headers(
    document: &CsvDocument,
) -> Vec<String> {
    let mut normalized: Vec<String> = document
        .headers
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();

    normalized.sort();
    normalized
}

/// Flags any confirmed unit file whose headers don't match the majority
/// shape among the rest of the confirmed set -- the assumption being that
/// every confirmed file comes from the same vendor/export tool, so a
/// genuine mismatch signals something's actually wrong with the
/// selection (a stray file from a different facility/tool got checked)
/// rather than being a normal, expected state. Ties for "majority" are
/// broken arbitrarily (whichever group `HashMap` iteration visits first)
/// -- with a real tie there's no principled way to prefer one shape over
/// the other anyway.
pub(crate) fn find_header_mismatches(
    documents: &[&CsvDocument],
) -> Vec<String> {
    if documents.len() <= 1 {
        return Vec::new();
    }

    let mut groups: std::collections::HashMap<Vec<String>, Vec<String>> =
        std::collections::HashMap::new();

    for document in documents {
        let normalized = normalized_headers(document);

        groups
            .entry(normalized)
            .or_default()
            .push(document.file_name.clone());
    }

    if groups.len() <= 1 {
        return Vec::new();
    }

    let majority_key = groups
        .iter()
        .max_by_key(|(_, files)| files.len())
        .map(|(key, _)| key.clone());

    let mut mismatched: Vec<String> = groups
        .into_iter()
        .filter(|(key, _)| Some(key) != majority_key.as_ref())
        .flat_map(|(_, files)| files)
        .collect();

    mismatched.sort();
    mismatched
}

/// A real master group file has either the minimal set (Name,
/// Description, Active) or the full set (Name, Description, Assigned
/// To, Status, Last Updated) -- either is accepted, and column order
/// never matters (`header_index` is a set lookup, not positional). Extra
/// columns beyond either set are simply ignored.
pub(crate) fn is_group_document(
    document: &CsvDocument,
) -> bool {
    const MINIMAL: [&str; 3] =
        ["name", "description", "active"];

    const FULL: [&str; 5] = [
        "name",
        "description",
        "assignedto",
        "status",
        "lastupdated",
    ];

    let has_all = |required: &[&str]| {
        required.iter().all(|r| {
            document
                .header_index(r)
                .is_some()
        })
    };

    has_all(&MINIMAL) || has_all(&FULL)
}
