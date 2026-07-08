// Building a BatchRun (per-facility and cross-facility group inventories)
// from the discovered unit CSV documents.

use std::collections::HashMap;

use anyhow::Result;

use crate::domain::csv_document::CsvDocument;
use crate::domain::models::{BatchRun, Facility};

pub fn build_batch_from_documents(
    unit_docs: Vec<&CsvDocument>,
) -> Result<BatchRun> {
    let mut facilities =
        Vec::<Facility>::new();

    let mut global_groups =
        HashMap::<String, usize>::new();

    for document in unit_docs {
        let group_index = match document
            .headers
            .iter()
            .position(|h| {
                h.to_lowercase()
                    == "unitgroup"
            }) {
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
