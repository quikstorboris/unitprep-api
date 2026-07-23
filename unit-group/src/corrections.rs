// Session-level overlay of manually-corrected cell values, applied on top
// of the originally parsed documents rather than mutating them in place —
// keeps `session.data.documents` a stable, cheaply-Arc-shared record of
// what was actually uploaded, with corrections layered on read instead.
// See Session::effective_documents, which is what validation/analysis/export
// should read through instead of `session.data.documents` directly.

use std::collections::HashMap;

use unitprep_core::csv_document::CsvDocument;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CorrectionKey {
    pub file_name: String,
    pub unit_number: String,
    pub field: String,
}

/// A unit deliberately excluded from the "Invalid dimensions" check — for
/// catalog entries that aren't real dimensioned storage units (an office,
/// an owner's apartment, etc.), where a blank Width/Length is correct,
/// not a data problem. Distinct from `CorrectionKey`: this isn't a
/// corrected value, it's an instruction to stop checking a value at all.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DimensionExemptionKey {
    pub file_name: String,
    pub unit_number: String,
}

/// Returns `document` with any matching corrections applied to their cells.
/// Rows are matched by the file's own "number" column; the field name is
/// resolved to a column via the same case-insensitive header lookup used
/// everywhere else (`CsvDocument::header_index`).
pub fn apply_corrections(
    document: &CsvDocument,
    corrections: &HashMap<
        CorrectionKey,
        String,
    >,
) -> CsvDocument {
    if corrections.is_empty() {
        return document.clone();
    }

    let Some(number_index) =
        document.header_index("number")
    else {
        return document.clone();
    };

    let mut result = document.clone();

    for row in &mut result.rows {
        let Some(unit_number) = row
            .get(number_index)
            .cloned()
        else {
            continue;
        };

        for (key, value) in corrections {
            if key.file_name
                != document.file_name
                || key.unit_number
                    != unit_number
            {
                continue;
            }

            let Some(field_index) =
                document.header_index(
                    &key.field,
                )
            else {
                continue;
            };

            if let Some(cell) =
                row.get_mut(field_index)
            {
                *cell = value.clone();
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document() -> CsvDocument {
        CsvDocument {
            modified_at: None,
            file_name: "units.csv"
                .to_string(),
            headers: vec![
                "number".to_string(),
                "unitgroup".to_string(),
                "width".to_string(),
            ],
            rows: vec![
                vec![
                    "A01".to_string(),
                    "10x10 Inside Climate"
                        .to_string(),
                    "0".to_string(),
                ],
                vec![
                    "A02".to_string(),
                    "10x10 Inside Climate"
                        .to_string(),
                    "10".to_string(),
                ],
            ],
        }
    }

    #[test]
    fn applies_correction_to_matching_unit_and_field(
    ) {
        let mut corrections =
            HashMap::new();

        corrections.insert(
            CorrectionKey {
                file_name: "units.csv"
                    .to_string(),
                unit_number: "A01"
                    .to_string(),
                field: "width"
                    .to_string(),
            },
            "10".to_string(),
        );

        let corrected =
            apply_corrections(
                &document(),
                &corrections,
            );

        assert_eq!(
            corrected.rows[0][2],
            "10"
        );

        // Untouched row stays untouched.
        assert_eq!(
            corrected.rows[1][2],
            "10"
        );
    }

    #[test]
    fn ignores_corrections_for_a_different_file(
    ) {
        let mut corrections =
            HashMap::new();

        corrections.insert(
            CorrectionKey {
                file_name:
                    "other.csv"
                        .to_string(),
                unit_number: "A01"
                    .to_string(),
                field: "width"
                    .to_string(),
            },
            "999".to_string(),
        );

        let corrected =
            apply_corrections(
                &document(),
                &corrections,
            );

        assert_eq!(
            corrected.rows[0][2],
            "0"
        );
    }

    #[test]
    fn empty_corrections_returns_document_unchanged(
    ) {
        let corrected =
            apply_corrections(
                &document(),
                &HashMap::new(),
            );

        assert_eq!(
            corrected.rows,
            document().rows
        );
    }
}
