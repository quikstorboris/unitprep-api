// Selecting, then loading, the master/reference Unit Group list that
// facility groups are compared against.

use anyhow::Result;

use unitprep_core::csv_document::CsvDocument;

use crate::domain::session::DiscoveryResult;

/// Selects which of the session's documents is the master/reference
/// group file to use for analysis: the explicitly selected file if the
/// operator picked one (via `/group-file/select`, needed when discovery
/// found more than one candidate), otherwise whichever single document
/// discovery classified as a group file. Business logic, not HTTP
/// orchestration — moved out of the `/analyze` handler for that reason,
/// not just to shrink it.
pub fn select_group_document<'a>(
    documents: &'a [CsvDocument],
    discovery: &DiscoveryResult,
) -> Option<&'a CsvDocument> {
    match &discovery
        .selected_group_file_name
    {
        Some(selected_file) => {
            tracing::info!(
                file = %selected_file,
                "Using selected master group file"
            );

            documents.iter().find(|d| {
                d.file_name
                    == *selected_file
            })
        }

        None => documents.iter().find(
            |d| {
                discovery
                    .group_file_names
                    .contains(
                        &d.file_name,
                    )
            },
        ),
    }
}

pub fn load_reference_groups_from_document(
    document: &CsvDocument,
) -> Result<Vec<String>> {
    let name_index =
        document
            .headers
            .iter()
            .position(|h| {
                h.to_lowercase()
                    == "name"
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Group file '{}' is missing a Name column",
                    document.file_name
                )
            })?;

    let groups = document
        .rows
        .iter()
        .filter_map(|row| {
            row.get(name_index)
        })
        .map(|v| {
            v.trim()
                .to_string()
        })
        .filter(|v| {
            !v.is_empty()
        })
        .collect();

    Ok(groups)
}
