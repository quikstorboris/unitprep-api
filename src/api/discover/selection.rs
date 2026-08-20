//! Unit-file and group-file selection/readiness resolution, plus the
//! discovered-group-names view derived once a unit file selection is
//! settled. Split out of `compute.rs` — `compute_discovery` orchestrates
//! these three self-contained resolution steps (each with its own
//! result type) rather than owning their logic inline, so the top-level
//! function reads as a sequence of named steps instead of one long body
//! mixing classification, group-file gating, and group-name extraction
//! together.

use unitprep_core::csv_document::CsvDocument;
use unitprep_core::vendor_format::{detect_vendor, VendorFormat};
use unitprep_unit_group::{
    build_batch_from_documents, is_uncommon_group_name, DiscoveryResult, UnitFileCandidate,
};

use crate::application::unit_group_session::Session;

use super::format_helpers::is_group_document;

/// The result of classifying every uploaded document into a unit-file
/// candidate, a group-file candidate, or unrecognized, then reconciling
/// that against any previously confirmed unit-file selection.
pub(super) struct UnitFileSelection {
    pub(super) candidates: Vec<UnitFileCandidate>,
    pub(super) group_files: Vec<String>,
    pub(super) selected_names: Vec<String>,
    pub(super) requires_selection: bool,
}

pub(super) fn reconcile_unit_file_selection(
    session: &Session,
    previous: &Option<DiscoveryResult>,
    unit_vendors: &[VendorFormat],
) -> UnitFileSelection {
    let mut candidates: Vec<UnitFileCandidate> = Vec::new();
    let mut group_files: Vec<String> = Vec::new();
    let mut unrecognized_count = 0usize;

    for document in session.data.documents.iter() {
        if let Some(vendor) = detect_vendor(document, unit_vendors) {
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
pub(super) struct GroupFileReadiness {
    pub(super) selected_name: Option<String>,
    pub(super) format_valid: Option<bool>,
    pub(super) ready: bool,
}

pub(super) fn resolve_group_file_readiness(
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
pub(super) fn compute_discovered_group_names(
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
