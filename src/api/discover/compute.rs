//! `compute_discovery` and the pieces it's built from. Shared by
//! `/discover`, `/unit-file/select`, and `/unit-file/resolve-format` —
//! all three mutate some piece of session state and then need the same
//! recomputed discovery view back.

use unitprep_core::csv_document::CsvDocument;
use unitprep_core::vendor_format::{detect_vendor, VendorFormat};
use unitprep_unit_group::{
    mapping_from_vendor, DiscoveryResult, FieldMappingEntry, CANONICAL_TARGET_FIELDS,
    REQUIRED_TARGET_FIELDS,
};

use crate::application::unit_group_session::Session;

use super::dto::DiscoverResponse;
use super::format_helpers::find_header_mismatches;
use super::selection::{
    compute_discovered_group_names, reconcile_unit_file_selection, resolve_group_file_readiness,
};

/// Classifies every document in `session`, resolves unit/group file
/// selection against any prior selection still valid, stores the result
/// on the session, and returns the API-facing response for it.
///
/// `unit_vendors` is loaded from `client_ops.vendor_format` by the
/// caller (an async DB read) before entering the session lock this runs
/// inside — that lock's own closure is synchronous, so the load can't
/// happen in here.
pub(crate) fn compute_discovery(
    session: &mut Session,
    unit_vendors: &[VendorFormat],
) -> DiscoverResponse {
    let previous = session.data.discovery.clone();

    // Stashed for `Session::effective_documents`'s own auto-detect
    // fallback, used by callers well outside this discovery flow
    // (validate/analyze/correct/...) that have no reason to thread the
    // registry through themselves — see `SessionData::unit_vendors`'s
    // own doc comment.
    session.data.unit_vendors = unit_vendors.to_vec();

    let selection = reconcile_unit_file_selection(session, &previous, unit_vendors);

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
        Some(document) => match detect_vendor(document, unit_vendors) {
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
            // A candidate matched a vendor signature to become one at
            // all, but a file forced in via `/unit-file/upload` bypasses
            // that -- real case now, not hypothetical. Falls back to the
            // document's own headers with no suggested mapping, which is
            // exactly what the manual-mapping UI needs.
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
                .and_then(|document| detect_vendor(document, unit_vendors))
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
