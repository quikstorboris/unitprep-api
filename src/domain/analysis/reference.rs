// Loading the master/reference Unit Group list that facility groups are
// compared against.

use anyhow::Result;

use unitprep_core::csv_document::CsvDocument;

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
