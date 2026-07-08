// Aggregate-level checks. Unlike row_checks, these only make sense after
// every row has been scanned once — they operate on the counts/groupings
// accumulated during that scan, not on any single row. mod.rs calls
// these once, after its row loop finishes.

use std::collections::HashMap;

/// Group names that appear on exactly one unit in this file.
///
/// Feeds both the "rare group" and "single-unit group" issues in mod.rs
/// — those are the same underlying fact surfaced as two distinct
/// user-facing messages, so the set is computed once here rather than
/// accumulated twice during the row scan.
pub(super) fn single_occurrence_groups(
    group_counts: &HashMap<String, usize>,
) -> Vec<String> {
    group_counts
        .iter()
        .filter(|(_, &count)| count == 1)
        .map(|(group, _)| group.clone())
        .collect()
}

/// Unit numbers that appear on more than one row, sorted.
pub(super) fn duplicate_units(
    unit_counts: HashMap<String, usize>,
) -> Vec<String> {
    let mut duplicates: Vec<String> =
        unit_counts
            .into_iter()
            .filter(|(_, count)| {
                *count > 1
            })
            .map(|(unit, _)| unit)
            .collect();

    duplicates.sort();
    duplicates
}

/// Unit numbers seen written with more than one distinct casing (e.g.
/// "K10" and "k10" both appearing) — flags every variant seen.
pub(super) fn casing_inconsistencies(
    casing_map: HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut flagged = Vec::new();

    for mut variants in
        casing_map.into_values()
    {
        variants.sort();
        variants.dedup();

        if variants.len() > 1 {
            flagged.extend(variants);
        }
    }

    flagged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(
        pairs: &[(&str, usize)],
    ) -> HashMap<String, usize> {
        pairs
            .iter()
            .map(|(k, v)| {
                (k.to_string(), *v)
            })
            .collect()
    }

    #[test]
    fn single_occurrence_groups_excludes_groups_seen_more_than_once() {
        let group_counts = counts(&[
            ("rare-group", 1),
            ("common-group", 5),
        ]);

        let result =
            single_occurrence_groups(
                &group_counts,
            );

        assert_eq!(
            result,
            vec![
                "rare-group".to_string()
            ]
        );
    }

    #[test]
    fn duplicate_units_are_sorted_and_singles_excluded() {
        let unit_counts = counts(&[
            ("B02", 2),
            ("A01", 1),
            ("C03", 3),
        ]);

        let result =
            duplicate_units(unit_counts);

        assert_eq!(
            result,
            vec![
                "B02".to_string(),
                "C03".to_string(),
            ]
        );
    }

    #[test]
    fn casing_inconsistencies_flags_only_multi_casing_units() {
        let mut casing_map: HashMap<
            String,
            Vec<String>,
        > = HashMap::new();

        casing_map.insert(
            "k10".to_string(),
            vec![
                "K10".to_string(),
                "k10".to_string(),
            ],
        );

        casing_map.insert(
            "a01".to_string(),
            vec!["A01".to_string()],
        );

        let mut result =
            casing_inconsistencies(
                casing_map,
            );

        result.sort();

        assert_eq!(
            result,
            vec![
                "K10".to_string(),
                "k10".to_string(),
            ]
        );
    }
}
