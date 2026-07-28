//! `compute_discovery` and the pieces it's built from. Shared by
//! `/discover`, `/unit-file/select`, and `/unit-file/resolve-format` —
//! all three mutate some piece of session state and then need the same
//! recomputed discovery view back.

use unitprep_core::csv_document::CsvDocument;
use unitprep_unit_group::{
    build_batch_from_documents, detect_vendor, is_uncommon_group_name, mapping_from_vendor,
    DiscoveryResult, FieldMappingEntry, UnitFileCandidate, CANONICAL_TARGET_FIELDS,
    REQUIRED_TARGET_FIELDS,
};

use crate::application::unit_group_session::Session;

use super::dto::DiscoverResponse;
use super::format_helpers::{find_header_mismatches, is_group_document};

/// Classifies every document in `session`, resolves unit/group file
/// selection against any prior selection still valid, stores the result
/// on the session, and returns the API-facing response for it.
pub(crate) fn compute_discovery(session: &mut Session) -> DiscoverResponse {
    let previous = session.data.discovery.clone();

    let selection = reconcile_unit_file_selection(session, &previous);

    let group_readiness = resolve_group_file_readiness(session, &selection.group_files, &previous);

    let group_file_confirmed = session.data.group_file_confirmed;

    // Which confirmed file the confirm/map UI works on next -- sorted so
    // repeated calls (and /unit-file/resolve-format, which shares this
    // same "current" notion via `current_unit_file_to_resolve`) always
    // agree on the order regardless of upload order.
    let mut pending_unit_file_names: Vec<String> = selection
        .selected_names
        .iter()
        .filter(|name| !session.data.format_resolutions.contains_key(*name))
        .cloned()
        .collect();

    pending_unit_file_names.sort();

    let selected_documents: Vec<&CsvDocument> = session
        .data
        .documents
        .iter()
        .filter(|d| selection.selected_names.contains(&d.file_name))
        .collect();

    let mismatched_header_files = find_header_mismatches(&selected_documents);

    let requires_format_resolution = !pending_unit_file_names.is_empty();

    let current_unit_file_name = pending_unit_file_names.first().cloned();

    let current_document = current_unit_file_name
        .as_ref()
        .and_then(|name| session.data.documents.iter().find(|d| &d.file_name == name));

    let (detected_vendor_name, source_headers, suggested_mapping) = match current_document {
        Some(document) => match detect_vendor(document) {
            Some(vendor) => {
                let suggested: Vec<FieldMappingEntry> = mapping_from_vendor(vendor)
                    .into_iter()
                    .filter_map(|(target, source)| {
                        source.map(|source| FieldMappingEntry { target, source })
                    })
                    .collect();

                (
                    Some(vendor.name.to_string()),
                    document.headers.clone(),
                    suggested,
                )
            }
            // Unreachable today -- every unit file candidate matched a
            // vendor signature to become one at all -- kept correct
            // (populated headers, not silently empty) rather than
            // assuming it can never happen.
            None => (None, document.headers.clone(), Vec::new()),
        },
        None => (None, Vec::new(), Vec::new()),
    };

    let unit_file_names = selection.selected_names.clone();

    // Group names are extracted purely from the confirmed+resolved unit
    // files -- independent of the master group file's own state -- so
    // they can (and per Boris's request, should) be shown as soon as the
    // unit-file steps are done, not gated behind the whole session being
    // `ready` (which also waits on the group file).
    let unit_files_resolved = !unit_file_names.is_empty()
        && !selection.requires_selection
        && !requires_format_resolution
        && mismatched_header_files.is_empty();

    let ready = unit_files_resolved && group_readiness.ready;

    let mut sorted_selected_unit_file_names = unit_file_names.clone();
    sorted_selected_unit_file_names.sort();

    // `detected_vendor_name` goes back to `None` once every confirmed
    // file is resolved (there's no more "current" pending file for it to
    // describe) -- re-derive the vendor for display purposes from any
    // one of the now-resolved documents instead of tracking it
    // separately through the bulk-confirm action.
    let confirmed_vendor_name = if !requires_format_resolution {
        sorted_selected_unit_file_names.iter().find_map(|name| {
            session
                .data
                .documents
                .iter()
                .find(|d| &d.file_name == name)
                .and_then(detect_vendor)
                .map(|vendor| vendor.name.to_string())
        })
    } else {
        None
    };

    // Shown as soon as Unit Files Selected is confirmed -- deliberately
    // *not* gated on `unit_files_resolved` (full format confirmation)
    // too. Each selected file's vendor is already known at this point
    // (the same `detect_vendor` call that named it in
    // `unit_file_candidates` above), so there's nothing left to wait on
    // for the common case. A file matching no known vendor and without
    // a stored resolution yet (needs manual mapping) simply contributes
    // no groups until it's resolved -- not an error, just nothing to
    // extract yet.
    let group_names_ready = !unit_file_names.is_empty() && !selection.requires_selection;

    let (discovered_group_names, uncommon_group_names) =
        compute_discovered_group_names(session, &unit_file_names, group_names_ready);

    let discovery = DiscoveryResult {
        unit_file_names: unit_file_names.clone(),
        group_file_names: selection.group_files.clone(),
        selected_group_file_name: group_readiness.selected_name.clone(),
        ready,
        unit_file_candidates: selection.candidates.clone(),
        selected_unit_file_names: selection.selected_names.clone(),
        requires_unit_file_selection: selection.requires_selection,
        requires_format_resolution,
        current_unit_file_name: current_unit_file_name.clone(),
        pending_unit_file_names: pending_unit_file_names.clone(),
        detected_vendor_name: detected_vendor_name.clone(),
        source_headers: source_headers.clone(),
        suggested_mapping: suggested_mapping.clone(),
    };

    session.complete_discovery(discovery.clone());

    // Every field `DiscoverResponse` shares with `DiscoveryResult`
    // (group_file_names, unit_file_candidates, ready, etc.) comes from
    // the `From` impl below; only the fields that need data
    // `DiscoveryResult` doesn't carry are set explicitly here.
    DiscoverResponse {
        group_file_format_valid: group_readiness.format_valid,
        group_file_confirmed,
        discovered_group_names,
        uncommon_group_names,
        mismatched_header_files,
        confirmed_vendor_name,
        canonical_target_fields: CANONICAL_TARGET_FIELDS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        required_target_fields: REQUIRED_TARGET_FIELDS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        ..DiscoverResponse::from(&discovery)
    }
}

/// The result of classifying every uploaded document into a unit-file
/// candidate, a group-file candidate, or unrecognized, then reconciling
/// that against any previously confirmed unit-file selection.
struct UnitFileSelection {
    candidates: Vec<UnitFileCandidate>,
    group_files: Vec<String>,
    selected_names: Vec<String>,
    requires_selection: bool,
}

fn reconcile_unit_file_selection(
    session: &Session,
    previous: &Option<DiscoveryResult>,
) -> UnitFileSelection {
    let mut candidates: Vec<UnitFileCandidate> = Vec::new();
    let mut group_files: Vec<String> = Vec::new();
    let mut unrecognized_count = 0usize;

    for document in session.data.documents.iter() {
        if let Some(vendor) = detect_vendor(document) {
            candidates.push(UnitFileCandidate {
                file_name: document.file_name.clone(),
                modified_at: document.modified_at,
                detected_vendor: vendor.name.to_string(),
            });
        } else if is_group_document(document) {
            group_files.push(document.file_name.clone());
        } else {
            unrecognized_count += 1;
        }
    }

    tracing::info!(
        session_id = %session.metadata.id,
        unrecognized_files = unrecognized_count,
        "Classified discovered documents"
    );

    // Preserve a previously confirmed selection, dropping any file that no
    // longer classifies as a candidate (e.g. the underlying document
    // changed enough to stop matching a vendor signature) rather than
    // discarding the whole selection over one stale entry.
    let previous_selection: Vec<String> = previous
        .as_ref()
        .map(|d| d.selected_unit_file_names.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|name| candidates.iter().any(|c| &c.file_name == name))
        .collect();

    let selected_names: Vec<String> = if !previous_selection.is_empty() {
        previous_selection
    } else if candidates.len() == 1 {
        // Nothing to choose between -- the one candidate found is used.
        vec![candidates[0].file_name.clone()]
    } else {
        Vec::new()
    };

    let requires_selection = candidates.len() > 1 && selected_names.is_empty();

    UnitFileSelection {
        candidates,
        group_files,
        selected_names,
        requires_selection,
    }
}

/// The master group file's selection/validity/confirmation state, and
/// whether it's ready to proceed.
struct GroupFileReadiness {
    selected_name: Option<String>,
    format_valid: Option<bool>,
    ready: bool,
}

fn resolve_group_file_readiness(
    session: &Session,
    group_files: &[String],
    previous: &Option<DiscoveryResult>,
) -> GroupFileReadiness {
    // Zero candidate master group files is a legitimate, ready-to-proceed
    // state (a net-new client with nothing in QMS yet to
    // cross-reference against) — analysis already handles a `None`
    // reference set by treating every discovered group as net-new.
    let selected_name = if group_files.len() == 1 {
        Some(group_files[0].clone())
    } else {
        previous
            .as_ref()
            .and_then(|d| d.selected_group_file_name.clone())
            // Same reasoning as unit-file selection above: a file
            // forced via `/group-file/upload` may not independently
            // classify as a group document, but must still survive this
            // recompute as long as the document itself still exists.
            .filter(|name| session.data.documents.iter().any(|d| &d.file_name == name))
    };

    let format_valid: Option<bool> = selected_name
        .as_ref()
        .and_then(|name| session.data.documents.iter().find(|d| &d.file_name == name))
        .map(is_group_document);

    // Satisfied either by the deliberate "net-new client" path (truly
    // zero group files ever classified, nothing to select -- gated
    // purely by the frontend's own acknowledgment, same as before) or by
    // an explicit confirmation of a valid selected file. A file that
    // IS selected but invalid or unconfirmed blocks readiness
    // regardless of how many candidates were auto-classified --
    // ambiguity among multiple auto-detected candidates no longer
    // matters on its own, since the user resolves it by explicitly
    // selecting and confirming one via the same select-file flow used
    // for the zero-candidates case.
    let ready = match &selected_name {
        Some(_) => session.data.group_file_confirmed && format_valid != Some(false),
        None => group_files.is_empty(),
    };

    GroupFileReadiness {
        selected_name,
        format_valid,
        ready,
    }
}

/// Distinct UnitGroup values across the confirmed+resolved unit files,
/// plus the subset that look uncommon. Reads through
/// `Session::effective_documents()` (format mapping + corrections +
/// exclusions) rather than reimplementing that view here — this used to
/// hand-roll the first two steps and omit the third, so an excluded
/// group could still show up in this display-only list even though
/// every other stage of the pipeline had already dropped it.
fn compute_discovered_group_names(
    session: &Session,
    unit_file_names: &[String],
    group_names_ready: bool,
) -> (Vec<String>, Vec<String>) {
    if !group_names_ready {
        return (Vec::new(), Vec::new());
    }

    let effective: Vec<CsvDocument> = session.effective_documents_for(unit_file_names);

    let selected: Vec<&CsvDocument> = effective.iter().collect();

    let mut names: Vec<String> = build_batch_from_documents(selected)
        .map(|batch| batch.global_groups.into_keys().collect())
        .unwrap_or_default();

    names.sort();

    let uncommon: Vec<String> = names
        .iter()
        .filter(|name| is_uncommon_group_name(name))
        .cloned()
        .collect();

    (names, uncommon)
}
