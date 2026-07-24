use std::time::Instant;

use axum::{
    extract::{Json, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use unitprep_core::session_store::SessionStoreExt;

use crate::api::{session_not_found, AppState};
use crate::application::unit_group_session::Session;
use unitprep_unit_group::{
    apply_corrections,
    apply_field_mapping,
    build_batch_from_documents,
    detect_vendor,
    is_uncommon_group_name,
    mapping_from_vendor,
    DiscoveryResult,
    FieldMappingEntry,
    UnitFileCandidate,
    CANONICAL_TARGET_FIELDS,
    REQUIRED_TARGET_FIELDS,
};

#[derive(Debug, Deserialize)]
pub struct DiscoverRequest {
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct DiscoverResponse {
    pub unit_files_found: usize,
    pub group_files_found: usize,
    pub group_file_names: Vec<String>,
    pub selected_group_file_name:
        Option<String>,
    /// Whether the currently selected group file actually looks like a
    /// real master group file (the same `name`/`description`/
    /// `assignedto`/`status`/`lastupdated` header check discovery itself
    /// uses to classify one automatically) — meaningful mainly for a
    /// manually-uploaded override (see `/group-file/upload`), which
    /// bypasses that classification on purpose. `None` until a group
    /// file is selected at all.
    pub group_file_format_valid:
        Option<bool>,
    /// Explicit "yes, this is the right file" confirmation — see
    /// `/group-file/confirm`. Selecting a file (auto-detected or
    /// manual) is not enough on its own; `ready` requires this too.
    pub group_file_confirmed: bool,
    pub ready: bool,
    /// Distinct UnitGroup values found across the discovered unit
    /// files, sorted for stable display. Recomputed on every call
    /// rather than stored on `DiscoveryResult` — nothing downstream in
    /// the pipeline consumes it, it exists purely so the UI can show
    /// the user what it found before they commit to validate/export
    /// (most useful exactly when there's no master file to cross-check
    /// against yet). Empty until the selected unit file's format has
    /// been resolved (see `requires_format_resolution`) — a file whose
    /// vendor headers haven't been mapped to canonical columns yet has
    /// no `UnitGroup` column for this to read.
    pub discovered_group_names:
        Vec<String>,
    /// The subset of `discovered_group_names` that don't look like a real
    /// storage-unit group name (no parseable width/length dimension, or a
    /// degenerate 0x0) — a review hint, shown separately so it's easy to
    /// notice, never used to change matching/analysis behavior.
    pub uncommon_group_names:
        Vec<String>,

    /// Every discovered file matching a known vendor's header signature
    /// (QSX, DoorSwap, ...) — the checkbox list the frontend lets the
    /// user confirm a subset (or all) of.
    pub unit_file_candidates: Vec<UnitFileCandidate>,
    /// The confirmed set to actually process — same as `unit_file_names`,
    /// exposed under this name too since it's the one the unit-file
    /// selection/confirmation UI cares about.
    pub selected_unit_file_names: Vec<String>,
    /// More than one candidate and nothing confirmed yet — the frontend
    /// should show the checkbox picker (see `/unit-file/select`) before
    /// anything else.
    pub requires_unit_file_selection: bool,
    /// At least one confirmed file's vendor format hasn't been confirmed
    /// or manually mapped yet — the frontend should show the confirm/map
    /// screen (see `/unit-file/resolve-format`) for `current_unit_file_name`.
    pub requires_format_resolution: bool,
    /// Which confirmed file the confirm/map screen is currently working
    /// on — the first (by name) still missing a resolution. `None` once
    /// every confirmed file is resolved.
    pub current_unit_file_name: Option<String>,
    /// Every confirmed file still awaiting resolution, in the order
    /// they'll be resolved (`current_unit_file_name` is always this
    /// list's first entry) — lets the UI show progress without
    /// re-deriving it.
    pub pending_unit_file_names: Vec<String>,
    /// Confirmed files whose headers don't match the majority shape among
    /// the rest of the confirmed set — a safeguard against the
    /// supposedly-impossible case of the checkbox selection spanning more
    /// than one vendor/shape at once (see "Confirm {vendor}" bulk
    /// resolution below). Empty when everything's consistent, which
    /// should be every real case; non-empty blocks bulk confirmation
    /// until the user returns to Unit Files Selection and fixes it.
    pub mismatched_header_files: Vec<String>,
    pub detected_vendor_name: Option<String>,
    /// The vendor confirmed for the confirmed unit files, once every one
    /// of them is resolved (`detected_vendor_name` goes back to `None`
    /// at that point, since there's no longer a "current" pending file
    /// for it to describe) — derived by re-running vendor detection
    /// against any one of the selected documents, purely for display.
    /// `None` if formats aren't all resolved yet, or none of them
    /// matched a known vendor (all manually mapped).
    pub confirmed_vendor_name: Option<String>,
    /// `current_unit_file_name`'s own headers — only populated while
    /// `requires_format_resolution` is true, for building the manual
    /// mapping UI's per-target dropdowns.
    pub source_headers: Vec<String>,
    /// The detected vendor's preset mapping, to pre-fill the manual
    /// mapping UI (still fully overridable).
    pub suggested_mapping: Vec<FieldMappingEntry>,
    /// Static, session-independent: the full set of target fields the
    /// manual mapping UI's left column should list, and which of those
    /// are required. Same on every response — included here so the
    /// frontend never has to hard-code its own copy.
    pub canonical_target_fields: Vec<String>,
    pub required_target_fields: Vec<String>,
}

pub async fn discover(
    State(state): State<AppState>,
    Json(request): Json<DiscoverRequest>,
) -> Response {
    let started = Instant::now();

    let response = state
        .unit_group_sessions
        .with_session_mut(
            &request.session_id,
            |session| {
                let response = compute_discovery(session);

                tracing::info!(
                    session_id = %request.session_id,
                    unit_files_found = response.unit_files_found,
                    group_files_found = response.group_files_found,
                    requires_unit_file_selection = response.requires_unit_file_selection,
                    requires_format_resolution = response.requires_format_resolution,
                    group_file_confirmed = response.group_file_confirmed,
                    ready = response.ready,
                    discovery_ms =
                        started
                            .elapsed()
                            .as_millis(),
                    "Discovery complete"
                );

                response
            },
        );

    match response {
        Some(response) => {
            Json(response).into_response()
        }
        None => session_not_found(),
    }
}

/// Classifies every document in `session`, resolves unit/group file
/// selection against any prior selection still valid, stores the result
/// on the session, and returns the API-facing response for it. Shared by
/// `/discover`, `/unit-file/select`, and `/unit-file/resolve-format` — all
/// three mutate some piece of session state and then need the same
/// recomputed discovery view back.
pub(crate) fn compute_discovery(
    session: &mut Session,
) -> DiscoverResponse {
    let previous = session.data.discovery.clone();

    let mut unit_file_candidates: Vec<UnitFileCandidate> = Vec::new();
    let mut group_files: Vec<String> = Vec::new();
    let mut unrecognized_count = 0usize;

    for document in session.data.documents.iter() {
        if let Some(vendor) = detect_vendor(document) {
            unit_file_candidates.push(UnitFileCandidate {
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
        .filter(|name| unit_file_candidates.iter().any(|c| &c.file_name == name))
        .collect();

    let selected_unit_file_names: Vec<String> = if !previous_selection.is_empty() {
        previous_selection
    } else if unit_file_candidates.len() == 1 {
        // Nothing to choose between -- the one candidate found is used.
        vec![unit_file_candidates[0].file_name.clone()]
    } else {
        Vec::new()
    };

    let requires_unit_file_selection =
        unit_file_candidates.len() > 1 && selected_unit_file_names.is_empty();

    // Zero candidate master group files is a legitimate, ready-to-proceed
    // state (a net-new client with nothing in QMS yet to
    // cross-reference against) — analysis already handles a `None`
    // reference set by treating every discovered group as net-new.
    let selected_group_file_name = if group_files.len() == 1 {
        Some(group_files[0].clone())
    } else {
        previous
            .as_ref()
            .and_then(|d| d.selected_group_file_name.clone())
            // Same reasoning as `selected_unit_file_name` above: a file
            // forced via `/group-file/upload` may not independently
            // classify as a group document, but must still survive this
            // recompute as long as the document itself still exists.
            .filter(|name| {
                session
                    .data
                    .documents
                    .iter()
                    .any(|d| &d.file_name == name)
            })
    };

    let group_file_format_valid: Option<bool> = selected_group_file_name
        .as_ref()
        .and_then(|name| {
            session
                .data
                .documents
                .iter()
                .find(|d| &d.file_name == name)
        })
        .map(is_group_document);

    let group_file_confirmed = session.data.group_file_confirmed;

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
    let group_file_ready = match &selected_group_file_name {
        Some(_) => {
            group_file_confirmed && group_file_format_valid != Some(false)
        }
        None => group_files.is_empty(),
    };

    // Which confirmed file the confirm/map UI works on next -- sorted so
    // repeated calls (and /unit-file/resolve-format, which shares this
    // same "current" notion via `current_unit_file_to_resolve`) always
    // agree on the order regardless of upload order.
    let mut pending_unit_file_names: Vec<String> = selected_unit_file_names
        .iter()
        .filter(|name| {
            !session
                .data
                .format_resolutions
                .contains_key(*name)
        })
        .cloned()
        .collect();

    pending_unit_file_names.sort();

    let selected_documents: Vec<&unitprep_core::csv_document::CsvDocument> = session
        .data
        .documents
        .iter()
        .filter(|d| selected_unit_file_names.contains(&d.file_name))
        .collect();

    let mismatched_header_files = find_header_mismatches(&selected_documents);

    let requires_format_resolution = !pending_unit_file_names.is_empty();
    let current_unit_file_name = pending_unit_file_names.first().cloned();

    let current_document = current_unit_file_name.as_ref().and_then(|name| {
        session
            .data
            .documents
            .iter()
            .find(|d| &d.file_name == name)
    });

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

    let unit_file_names = selected_unit_file_names.clone();

    // Group names are extracted purely from the confirmed+resolved unit
    // files -- independent of the master group file's own state -- so
    // they can (and per Boris's request, should) be shown as soon as the
    // unit-file steps are done, not gated behind the whole session being
    // `ready` (which also waits on the group file).
    let unit_files_resolved = !unit_file_names.is_empty()
        && !requires_unit_file_selection
        && !requires_format_resolution
        && mismatched_header_files.is_empty();

    let ready = unit_files_resolved && group_file_ready;

    let mut sorted_selected_unit_file_names = unit_file_names.clone();
    sorted_selected_unit_file_names.sort();

    // `detected_vendor_name` goes back to `None` once every confirmed
    // file is resolved (there's no more "current" pending file for it to
    // describe) -- re-derive the vendor for display purposes from any
    // one of the now-resolved documents instead of tracking it
    // separately through the bulk-confirm action.
    let confirmed_vendor_name = if !requires_format_resolution {
        sorted_selected_unit_file_names
            .iter()
            .find_map(|name| {
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
    let group_names_ready =
        !unit_file_names.is_empty() && !requires_unit_file_selection;

    let discovered_group_names: Vec<String> = if group_names_ready {
        let effective: Vec<unitprep_core::csv_document::CsvDocument> = session
            .data
            .documents
            .iter()
            .filter(|d| unit_file_names.contains(&d.file_name))
            .map(|document| {
                let mapped = match session
                    .data
                    .format_resolutions
                    .get(&document.file_name)
                {
                    Some(mapping) => apply_field_mapping(document, mapping),
                    None => match detect_vendor(document) {
                        Some(vendor) => {
                            apply_field_mapping(document, &mapping_from_vendor(vendor))
                        }
                        None => document.clone(),
                    },
                };

                apply_corrections(&mapped, &session.data.corrections)
            })
            .collect();

        let selected: Vec<&unitprep_core::csv_document::CsvDocument> =
            effective.iter().collect();

        let mut names: Vec<String> = build_batch_from_documents(selected)
            .map(|batch| batch.global_groups.into_keys().collect())
            .unwrap_or_default();

        names.sort();
        names
    } else {
        Vec::new()
    };

    let uncommon_group_names: Vec<String> = discovered_group_names
        .iter()
        .filter(|name| is_uncommon_group_name(name))
        .cloned()
        .collect();

    let discovery = DiscoveryResult {
        unit_file_names,
        group_file_names: group_files.clone(),
        selected_group_file_name: selected_group_file_name.clone(),
        ready,
        unit_file_candidates: unit_file_candidates.clone(),
        selected_unit_file_names: selected_unit_file_names.clone(),
        requires_unit_file_selection,
        requires_format_resolution,
        current_unit_file_name: current_unit_file_name.clone(),
        pending_unit_file_names: pending_unit_file_names.clone(),
        detected_vendor_name: detected_vendor_name.clone(),
        source_headers: source_headers.clone(),
        suggested_mapping: suggested_mapping.clone(),
    };

    session.complete_discovery(discovery.clone());

    DiscoverResponse {
        // Total candidates found, not just the confirmed subset —
        // meaningful even before a selection is made, unlike
        // `selected_unit_file_names` (which stays empty until confirmed).
        unit_files_found: discovery.unit_file_candidates.len(),
        group_files_found: discovery.group_file_names.len(),
        group_file_names: discovery.group_file_names.clone(),
        selected_group_file_name: discovery.selected_group_file_name.clone(),
        group_file_format_valid,
        group_file_confirmed,
        ready: discovery.ready,
        discovered_group_names,
        uncommon_group_names,
        unit_file_candidates: discovery.unit_file_candidates.clone(),
        selected_unit_file_names: discovery.selected_unit_file_names.clone(),
        requires_unit_file_selection: discovery.requires_unit_file_selection,
        requires_format_resolution: discovery.requires_format_resolution,
        current_unit_file_name: discovery.current_unit_file_name.clone(),
        pending_unit_file_names: discovery.pending_unit_file_names.clone(),
        mismatched_header_files,
        detected_vendor_name: discovery.detected_vendor_name.clone(),
        confirmed_vendor_name,
        source_headers: discovery.source_headers.clone(),
        suggested_mapping: discovery.suggested_mapping.clone(),
        canonical_target_fields: CANONICAL_TARGET_FIELDS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        required_target_fields: REQUIRED_TARGET_FIELDS
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

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

// Column presence is decided through `CsvDocument::header_index` — the
// single normalization rule every lookup in the system shares (see its
// doc comment) — rather than each caller building its own normalized
// header list. That's deliberate: this file previously had its own
// `normalize()` that stripped spaces/underscores while `header_index`
// only lowercased, so a header like "Unit_Group" could pass this check
// and then silently fail every subsequent lookup validation did.

/// Flags any confirmed unit file whose headers don't match the majority
/// shape among the rest of the confirmed set -- the assumption being that
/// every confirmed file comes from the same vendor/export tool, so a
/// genuine mismatch signals something's actually wrong with the
/// selection (a stray file from a different facility/tool got checked)
/// rather than being a normal, expected state. Ties for "majority" are
/// broken arbitrarily (whichever group `HashMap` iteration visits first)
/// -- with a real tie there's no principled way to prefer one shape over
/// the other anyway.
/// Order-insensitive header identity for comparing two documents' shapes
/// -- shared by `find_header_mismatches` below and
/// `resolve_unit_format`'s bulk-confirm logic, which needs the exact same
/// notion of "same shape" to decide which confirmed files a single
/// vendor confirmation covers.
pub(crate) fn normalized_headers(
    document: &unitprep_core::csv_document::CsvDocument,
) -> Vec<String> {
    let mut normalized: Vec<String> = document
        .headers
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();

    normalized.sort();
    normalized
}

pub(crate) fn find_header_mismatches(
    documents: &[&unitprep_core::csv_document::CsvDocument],
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
    document: &unitprep_core::csv_document::CsvDocument,
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


#[cfg(test)]
#[path = "discover_tests.rs"]
mod tests;
