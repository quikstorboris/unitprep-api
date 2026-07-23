// Building a BatchRun (per-facility and cross-facility group inventories)
// from the discovered unit CSV documents.

use std::collections::HashMap;

use anyhow::Result;

use unitprep_core::csv_document::CsvDocument;
use crate::models::{BatchRun, Facility};

pub fn build_batch_from_documents(
    unit_docs: Vec<&CsvDocument>,
) -> Result<BatchRun> {
    let mut facilities =
        Vec::<Facility>::new();

    let mut global_groups =
        HashMap::<String, usize>::new();

    for document in unit_docs {
        let group_index = match document
            .header_index("unitgroup")
        {
            Some(i) => i,

            None => {
                tracing::warn!(
                    file = %document.file_name,
                    "Unit file missing UnitGroup column — skipping"
                );

                continue;
            }
        };

        let mut groups =
            HashMap::<String, usize>::new();

        for row in &document.rows {
            if let Some(group) =
                row.get(group_index)
            {
                let group =
                    group.trim();

                if !group.is_empty()
                {
                    *groups
                        .entry(
                            group.to_string(),
                        )
                        .or_insert(0) += 1;

                    *global_groups
                        .entry(
                            group.to_string(),
                        )
                        .or_insert(0) += 1;
                }
            }
        }

        facilities.push(
            Facility {
                name: document
                    .file_name
                    .clone(),

                source_files:
                    vec![document
                        .file_name
                        .clone()],

                groups,
            },
        );
    }

    Ok(BatchRun {
        facilities,
        global_groups,
        advisory_issues:
            Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(headers: Vec<&str>, rows: Vec<Vec<&str>>) -> CsvDocument {
        CsvDocument {
            modified_at: None,
            file_name: "units.csv".to_string(),
            headers: headers.into_iter().map(str::to_string).collect(),
            rows: rows
                .into_iter()
                .map(|row| row.into_iter().map(str::to_string).collect())
                .collect(),
        }
    }

    #[test]
    fn counts_groups_by_exact_header_name() {
        let doc = document(
            vec!["number", "unitgroup"],
            vec![vec!["A01", "10x10 Climate"], vec!["A02", "10x10 Climate"]],
        );

        let batch = build_batch_from_documents(vec![&doc]).unwrap();

        assert_eq!(batch.global_groups.get("10x10 Climate"), Some(&2));
    }

    /// Regression: this lookup used to be a bespoke
    /// `h.to_lowercase() == "unitgroup"` check, which only lowercases —
    /// unlike `header_index` (case *and* separator insensitive), so a
    /// header like "Unit_Group" would silently miss it and the file
    /// would be skipped from analysis inventory even though discovery
    /// and validation already accepted it as a real unit file. Same bug
    /// class already fixed elsewhere (discover.rs, reference.rs).
    #[test]
    fn finds_the_group_column_regardless_of_separator_or_casing() {
        let doc = document(
            vec!["Number", "Unit_Group"],
            vec![vec!["A01", "10x10 Climate"]],
        );

        let batch = build_batch_from_documents(vec![&doc]).unwrap();

        assert_eq!(batch.global_groups.get("10x10 Climate"), Some(&1));
    }

    #[test]
    fn skips_a_file_with_no_group_column_instead_of_erroring() {
        let doc = document(vec!["number"], vec![vec!["A01"]]);

        let batch = build_batch_from_documents(vec![&doc]).unwrap();

        assert!(batch.facilities.is_empty());
        assert!(batch.global_groups.is_empty());
    }
}
