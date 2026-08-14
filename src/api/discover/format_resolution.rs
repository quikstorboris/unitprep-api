//! The unit-file confirm/map resolution logic behind
//! `/unit-file/resolve-format` (see `resolve_unit_format.rs`) -- bulk
//! vendor confirmation and manual-mapping validation both need
//! `find_header_mismatches`/`normalized_headers` from this module's own
//! `format_helpers` sibling, so they live here rather than in the
//! handler file itself.

use unitprep_unit_group::{
    detect_vendor, mapping_from_vendor, FieldMapping, CANONICAL_TARGET_FIELDS,
    REQUIRED_TARGET_FIELDS,
};

use crate::api::resolve_unit_format::{MappingEntryInput, ResolveNotReady};
use crate::application::unit_group_session::Session;

use super::{find_header_mismatches, normalized_headers};

/// Bulk-confirms a detected vendor across every currently-selected unit
/// file that shares `document`'s exact header shape -- one confirmation
/// resolves all of them at once instead of requiring a click per file
/// (see `find_header_mismatches`, which guarantees every selected file
/// shares this shape before this is ever called). Re-checking each
/// file's own headers here too (rather than just trusting that aggregate
/// result) costs nothing and means a future bug in the aggregate check
/// can't silently resolve a file it shouldn't.
pub(crate) fn resolve_confirm_action(
    session: &mut Session,
    session_id: &str,
    file_name: &str,
    document: &unitprep_core::csv_document::CsvDocument,
) -> Result<(), ResolveNotReady> {
    let vendor = match detect_vendor(document) {
        Some(vendor) => vendor,
        None => {
            tracing::warn!(
                session_id = %session_id,
                file = %file_name,
                "Confirm requested but vendor could not be detected"
            );

            return Err(ResolveNotReady::VendorNotDetected);
        }
    };

    let selected_unit_file_names = session
        .data
        .discovery
        .as_ref()
        .expect("Discovered stage guarantees discovery data")
        .selected_unit_file_names
        .clone();

    let selected_documents: Vec<_> = session
        .data
        .documents
        .iter()
        .filter(|d| selected_unit_file_names.contains(&d.file_name))
        .collect();

    let mismatched = find_header_mismatches(&selected_documents);

    if !mismatched.is_empty() {
        tracing::warn!(
            session_id = %session_id,
            mismatched_files = ?mismatched,
            "Bulk-confirm rejected — selected unit files don't share the same headers"
        );

        return Err(ResolveNotReady::HeaderMismatch(mismatched));
    }

    let mapping = mapping_from_vendor(vendor);
    let current_headers = normalized_headers(document);
    let mut resolved_files = Vec::new();

    for name in &selected_unit_file_names {
        if session.data.format_resolutions.contains_key(name) {
            continue;
        }

        let same_shape = session
            .data
            .documents
            .iter()
            .find(|d| &d.file_name == name)
            .is_some_and(|d| normalized_headers(d) == current_headers);

        if same_shape {
            session
                .data
                .format_resolutions
                .insert(name.clone(), mapping.clone());

            resolved_files.push(name.clone());
        }
    }

    for file_name in &resolved_files {
        tracing::info!(
            session_id = %session_id,
            vendor = vendor.name,
            file = %file_name,
            "Unit file format resolved (bulk-confirmed)"
        );
    }

    tracing::info!(
        session_id = %session_id,
        vendor = vendor.name,
        resolved_file_count = resolved_files.len(),
        "Unit file format bulk-confirm complete"
    );

    Ok(())
}

/// Validates a user-submitted manual mapping against the selected file's
/// own headers, then expands it into a `FieldMapping` covering every
/// canonical target field (unsubmitted targets map to `None`).
pub(crate) fn validate_manual_mapping(
    document: &unitprep_core::csv_document::CsvDocument,
    submitted: &[MappingEntryInput],
) -> Result<FieldMapping, ResolveNotReady> {
    for entry in submitted {
        if !CANONICAL_TARGET_FIELDS.contains(&entry.target.as_str()) {
            return Err(ResolveNotReady::UnknownTargetField(entry.target.clone()));
        }

        if let Some(source) = &entry.source {
            if document.header_index(source).is_none() {
                return Err(ResolveNotReady::UnknownSourceHeader {
                    target: entry.target.clone(),
                    source: source.clone(),
                });
            }
        }
    }

    let missing_required: Vec<String> = REQUIRED_TARGET_FIELDS
        .iter()
        .filter(|required| {
            !submitted
                .iter()
                .any(|entry| &entry.target == *required && entry.source.is_some())
        })
        .map(|s| s.to_string())
        .collect();

    if !missing_required.is_empty() {
        return Err(ResolveNotReady::MissingRequiredFields(missing_required));
    }

    Ok(CANONICAL_TARGET_FIELDS
        .iter()
        .map(|target| {
            let source = submitted
                .iter()
                .find(|entry| entry.target == *target)
                .and_then(|entry| entry.source.clone());

            (target.to_string(), source)
        })
        .collect())
}
