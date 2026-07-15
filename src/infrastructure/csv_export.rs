use std::collections::HashSet;
use std::io::{Cursor, Write};

use anyhow::Result;
use csv::Writer;
use zip::{
    write::SimpleFileOptions,
    CompressionMethod,
    ZipWriter,
};

use crate::domain::models::{AnalysisResults, Issue};

#[derive(Debug, Clone)]

/// Represents a single export artifact generated entirely
/// in memory.
///
/// Files are later assembled into a ZIP archive by the
/// export endpoint.
pub struct ExportFile {
    pub file_name: String,
    pub bytes: Vec<u8>,
}

/// Generates all UnitPrep export artifacts.
///
/// Returned files are:
///
/// - Deterministic
/// - Filesystem-independent
/// - Generated entirely in memory
///
/// This function intentionally contains the single source
/// of truth for export file generation.
pub fn generate_outputs(
    analysis: &AnalysisResults,
    advisory: bool,
) -> Result<Vec<ExportFile>> {
    let mut files = Vec::new();

    files.push(generate_net_new_groups_csv(analysis)?);

    files.push(generate_facility_assignments_csv(
        analysis,
    )?);

    files.push(generate_facility_group_lookup_csv(
        analysis,
    )?);

    if advisory {
        files.push(generate_advisory_csv(
            &analysis.batch_run.advisory_issues,
        )?);

        files.push(generate_advisory_json(
            &analysis.batch_run.advisory_issues,
        )?);
    }

    files.push(generate_batch_json(
        &analysis.batch_run,
    )?);

    Ok(files)
}

/// Packages generated export files into a ZIP archive, built entirely in
/// memory. Returns a plain `Result` rather than an HTTP response directly —
/// deciding how a failure here becomes an HTTP status is the caller's
/// concern (see `api::export`), not this module's. Keeping "how do we
/// package export artifacts" and "how do we respond to a client" as
/// separate concerns is why this lives here rather than in the handler:
/// this module already owns every other step of turning analysis results
/// into a deliverable.
pub fn build_zip(
    files: Vec<ExportFile>,
) -> Result<Vec<u8>> {
    let mut cursor =
        Cursor::new(Vec::<u8>::new());

    {
        let mut zip =
            ZipWriter::new(&mut cursor);

        let options =
            SimpleFileOptions::default()
                .compression_method(
                    CompressionMethod::Deflated,
                );

        for file in files {
            zip.start_file(
                &file.file_name,
                options,
            )
            .map_err(|err| {
                anyhow::anyhow!(
                    "Failed adding '{}' to ZIP: {}",
                    file.file_name,
                    err
                )
            })?;

            zip.write_all(&file.bytes)
                .map_err(|err| {
                    anyhow::anyhow!(
                        "Failed writing ZIP entry '{}': {}",
                        file.file_name,
                        err
                    )
                })?;
        }

        zip.finish().map_err(|err| {
            anyhow::anyhow!(
                "Failed finalizing ZIP: {}",
                err
            )
        })?;
    }

    Ok(cursor.into_inner())
}

fn generate_net_new_groups_csv(
    analysis: &AnalysisResults,
) -> Result<ExportFile> {
    let mut buffer = Vec::new();

    {
        let mut writer =
            Writer::from_writer(&mut buffer);

        writer.write_record([
            "Name",
            "Description",
            "Active",
        ])?;

        let mut rows =
            analysis.net_new_groups.clone();

        rows.sort();

        for group in rows {
            writer.write_record([
                &group,
                &group,
                "Yes",
            ])?;
        }

        writer.flush()?;
    }

    Ok(ExportFile {
        file_name: infer_output_name(
            analysis,
        ),
        bytes: buffer,
    })
}

fn generate_facility_assignments_csv(
    analysis: &AnalysisResults,
) -> Result<ExportFile> {
    let mut buffer = Vec::new();

    {
        let mut writer =
            Writer::from_writer(&mut buffer);

        writer.write_record([
            "Facility",
            "SourceFile",
            "UnitGroup",
            "UnitCount",
            "ExistsInGlobalGroups",
            "NetNew",
        ])?;

        let existing_set =
            analysis.reference_groups.as_ref().map(
                |groups| {
                    groups
                        .iter()
                        .cloned()
                        .collect::<HashSet<_>>()
                },
            );

        for facility in
            &analysis.batch_run.facilities
        {
            let mut groups: Vec<_> =
                facility.groups.iter().collect();

            groups.sort_by(
                |(left, _), (right, _)| {
                    left.cmp(right)
                },
            );

            for (group, count) in groups {
                let (exists, net_new) =
                    if let Some(set) =
                        &existing_set
                    {
                        let exists =
                            set.contains(group);

                        (
                            Some(exists),
                            Some(!exists),
                        )
                    } else {
                        (None, None)
                    };

                writer.write_record([
                    &facility.name,
                    &facility.source_files.join(
                        ";",
                    ),
                    group,
                    &count.to_string(),
                    &exists
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                    &net_new
                        .map(|v| v.to_string())
                        .unwrap_or_default(),
                ])?;
            }
        }

        writer.flush()?;
    }

    Ok(ExportFile {
        file_name:
            "facility_group_assignments.csv"
                .to_string(),
        bytes: buffer,
    })
}

fn generate_facility_group_lookup_csv(
    analysis: &AnalysisResults,
) -> Result<ExportFile> {
    let mut buffer = Vec::new();

    {
        let mut writer =
            Writer::from_writer(&mut buffer);

        writer.write_record([
            "Facility",
            "Group",
        ])?;

        let existing_set = analysis
            .reference_groups
            .as_ref()
            .map(|groups| {
                groups
                    .iter()
                    .cloned()
                    .collect::<HashSet<_>>()
            });

        for facility in
            &analysis.batch_run.facilities
        {
            let mut groups: Vec<_> =
                facility.groups.keys().collect();

            groups.sort();

            for group in groups {
                let is_net_new = existing_set
                    .as_ref()
                    .map(|set| {
                        !set.contains(group)
                    })
                    .unwrap_or(true);

                if is_net_new {
                    writer.write_record([
                        facility.name.as_str(),
                        group.as_str(),
                    ])?;
                }
            }
        }

        writer.flush()?;
    }

    Ok(ExportFile {
        file_name:
            "facility_group_lookup.csv"
                .to_string(),
        bytes: buffer,
    })
}

fn generate_advisory_csv(
    advisory_issues: &[Issue],
) -> Result<ExportFile> {
    let mut buffer = Vec::new();

    {
        let mut writer =
            Writer::from_writer(&mut buffer);

        writer.write_record([
            "Source",
            "Issue",
            "Severity",
        ])?;

        for issue in advisory_issues {
            writer.write_record([
                &issue.source,
                &issue.issue,
                &format!(
                    "{:?}",
                    issue.severity
                ),
            ])?;
        }

        writer.flush()?;
    }

    Ok(ExportFile {
        file_name:
            "advisory_issues.csv"
                .to_string(),
        bytes: buffer,
    })
}

fn generate_advisory_json(
    advisory_issues: &[Issue],
) -> Result<ExportFile> {
    Ok(ExportFile {
        file_name:
            "advisory_issues.json"
                .to_string(),
        bytes:
            serde_json::to_vec_pretty(
                advisory_issues,
            )?,
    })
}

fn generate_batch_json(
    batch: &crate::domain::models::BatchRun,
) -> Result<ExportFile> {
    Ok(ExportFile {
        file_name: "batch_run.json"
            .to_string(),
        bytes:
            serde_json::to_vec_pretty(
                batch,
            )?,
    })
}

fn infer_output_name(
    analysis: &AnalysisResults,
) -> String {
    if let Some(first_facility) =
        analysis.batch_run.facilities.first()
    {
        return format!(
            "{}_net_new_unit_groups.csv",
            sanitize_name(
                &first_facility.name
            )
        );
    }

    "net_new_unit_groups.csv".to_string()
}

fn sanitize_name(
    value: &str,
) -> String {
    value
        .trim_start_matches('_')
        .to_lowercase()
        .replace(' ', "_")
        .chars()
        .filter(|c| {
            c.is_ascii_alphanumeric()
                || *c == '_'
        })
        .collect()
}