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
    build_batch_from_documents,
    detect_vendor,
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
    pub requires_group_selection:
        bool,
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

    /// Every discovered file matching a known vendor's header signature
    /// (QSX, DoorSwap, ...) — a candidate to become this session's one
    /// selected unit file.
    pub unit_file_candidates: Vec<UnitFileCandidate>,
    pub selected_unit_file_name: Option<String>,
    /// More than one candidate and none selected yet — the frontend
    /// should show the file picker (see `/unit-file/select`) before
    /// anything else.
    pub requires_unit_file_selection: bool,
    /// Exactly one unit file selected, but its vendor format hasn't been
    /// confirmed or manually mapped yet — the frontend should show the
    /// confirm/map screen (see `/unit-file/resolve-format`).
    pub requires_format_resolution: bool,
    pub detected_vendor_name: Option<String>,
    /// The selected file's own headers — only populated while
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
                    requires_group_selection = response.requires_group_selection,
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
        unrecognized_files = unrecognized_count,
        "Classified discovered documents"
    );

    let selected_unit_file_name = previous
        .as_ref()
        .and_then(|d| d.selected_unit_file_name.clone())
        .filter(|name| {
            unit_file_candidates
                .iter()
                .any(|c| &c.file_name == name)
        })
        .or_else(|| {
            if unit_file_candidates.len() == 1 {
                Some(unit_file_candidates[0].file_name.clone())
            } else {
                None
            }
        });

    let requires_unit_file_selection =
        unit_file_candidates.len() > 1 && selected_unit_file_name.is_none();

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
            .filter(|name| group_files.contains(name))
    };

    let selected_document = selected_unit_file_name.as_ref().and_then(|name| {
        session
            .data
            .documents
            .iter()
            .find(|d| &d.file_name == name)
    });

    let already_resolved = selected_unit_file_name
        .as_ref()
        .is_some_and(|name| session.data.format_resolutions.contains_key(name));

    let (detected_vendor_name, source_headers, suggested_mapping, requires_format_resolution) =
        match (selected_document, already_resolved) {
            (Some(document), false) => match detect_vendor(document) {
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
                        true,
                    )
                }
                // Every unit file candidate matched a vendor signature to
                // get here at all — this branch existing at all would mean
                // detection disagreed with itself between passes.
                None => (None, Vec::new(), Vec::new(), false),
            },
            _ => (None, Vec::new(), Vec::new(), false),
        };

    let unit_file_names: Vec<String> =
        selected_unit_file_name.clone().into_iter().collect();

    let ready = !unit_file_names.is_empty()
        && !requires_unit_file_selection
        && !requires_format_resolution
        && group_files.len() <= 1;

    let discovered_group_names: Vec<String> = if ready || already_resolved {
        let effective = session.effective_documents();

        let selected: Vec<&unitprep_core::csv_document::CsvDocument> = effective
            .iter()
            .filter(|d| {
                selected_unit_file_name
                    .as_deref()
                    .is_some_and(|name| d.file_name == name)
            })
            .collect();

        let mut names: Vec<String> = build_batch_from_documents(selected)
            .map(|batch| batch.global_groups.into_keys().collect())
            .unwrap_or_default();

        names.sort();
        names
    } else {
        Vec::new()
    };

    let discovery = DiscoveryResult {
        unit_file_names,
        group_file_names: group_files.clone(),
        selected_group_file_name: selected_group_file_name.clone(),
        ready,
        unit_file_candidates: unit_file_candidates.clone(),
        selected_unit_file_name: selected_unit_file_name.clone(),
        requires_unit_file_selection,
        requires_format_resolution,
        detected_vendor_name: detected_vendor_name.clone(),
        source_headers: source_headers.clone(),
        suggested_mapping: suggested_mapping.clone(),
    };

    session.complete_discovery(discovery.clone());

    DiscoverResponse {
        // Total candidates found, not just the resolved/selected one(s) —
        // meaningful even before a selection is made, unlike
        // `unit_file_names` (which stays empty until something's
        // selected).
        unit_files_found: discovery.unit_file_candidates.len(),
        group_files_found: discovery.group_file_names.len(),
        group_file_names: discovery.group_file_names.clone(),
        selected_group_file_name: discovery.selected_group_file_name.clone(),
        requires_group_selection: discovery.group_file_names.len() > 1,
        ready: discovery.ready,
        discovered_group_names,
        unit_file_candidates: discovery.unit_file_candidates.clone(),
        selected_unit_file_name: discovery.selected_unit_file_name.clone(),
        requires_unit_file_selection: discovery.requires_unit_file_selection,
        requires_format_resolution: discovery.requires_format_resolution,
        detected_vendor_name: discovery.detected_vendor_name.clone(),
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

// Column presence is decided through `CsvDocument::header_index` — the
// single normalization rule every lookup in the system shares (see its
// doc comment) — rather than each caller building its own normalized
// header list. That's deliberate: this file previously had its own
// `normalize()` that stripped spaces/underscores while `header_index`
// only lowercased, so a header like "Unit_Group" could pass this check
// and then silently fail every subsequent lookup validation did.

fn is_group_document(
    document: &unitprep_core::csv_document::CsvDocument,
) -> bool {
    let required = [
        "name",
        "description",
        "assignedto",
        "status",
        "lastupdated",
    ];

    required.iter().all(|r| {
        document
            .header_index(r)
            .is_some()
    })
}


#[cfg(test)]
#[path = "discover_tests.rs"]
mod tests;
