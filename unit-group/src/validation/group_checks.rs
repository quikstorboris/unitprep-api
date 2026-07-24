// Aggregate-level checks. Unlike row_checks, these only make sense after
// every row has been scanned once — they operate on the counts/groupings
// accumulated during that scan, not on any single row. mod.rs calls
// these once, after its row loop finishes.

use std::collections::HashMap;

use crate::analysis::{
    has_malformed_dimension_attempt,
    is_uncommon_group_name,
};

/// Group names appearing on `max_occurrences` units or fewer in this
/// file, paired with their actual count — small enough that a
/// data-entry mistake (a typo, a wrong dimension) could easily be
/// lurking undetected among so few units of that type.
pub(super) fn rare_groups(
    group_counts: &HashMap<String, usize>,
    max_occurrences: usize,
) -> Vec<(String, usize)> {
    group_counts
        .iter()
        .filter(|(_, &count)| count <= max_occurrences)
        .map(|(group, &count)| (group.clone(), count))
        .collect()
}

/// A comma-merged value (usually a sign two group names got combined),
/// or, per `is_uncommon_group_name`, a name with no parseable
/// width/length dimension at all (or a degenerate 0x0) — pure
/// descriptive text, not a botched attempt at a dimension.
/// `is_uncommon_group_name` alone isn't quite this: it also returns true
/// for a name that *tried* to express a dimension and botched it ("10x",
/// missing its second number), since its strict regex simply fails to
/// match either way. `has_malformed_dimension_attempt` is the check that
/// actually distinguishes "tried and failed" from "never tried" (see its
/// own doc comment) — excluding that case here is what's needed so a
/// name is never both Odd and Invalid Dimensions (see `validation::mod`
/// for the other half of that rule). `is_uncommon_group_name` itself
/// stays untouched, since discovery's own "Uncommon Group Names" review
/// list calls it directly and the two pages' counts are meant to agree.
pub(super) fn is_odd_group_name(
    group: &str,
) -> bool {
    (group.contains(',')
        || is_uncommon_group_name(group))
        && !has_malformed_dimension_attempt(
            group,
        )
}

/// Distinct group names in this file that read as "odd" — see
/// `is_odd_group_name`.
pub(super) fn odd_group_names(
    group_counts: &HashMap<String, usize>,
) -> Vec<String> {
    group_counts
        .keys()
        .filter(|group| {
            is_odd_group_name(group)
        })
        .cloned()
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
    fn rare_groups_includes_everything_at_or_under_the_threshold() {
        let group_counts = counts(&[
            ("one-unit", 1),
            ("four-units", 4),
            ("five-units", 5),
        ]);

        let mut result =
            rare_groups(
                &group_counts,
                4,
            );

        result.sort();

        assert_eq!(
            result,
            vec![
                (
                    "four-units"
                        .to_string(),
                    4
                ),
                (
                    "one-unit"
                        .to_string(),
                    1
                ),
            ]
        );
    }

    #[test]
    fn odd_group_names_flags_comma_merged_and_dimensionless_names() {
        let group_counts = counts(&[
            ("10x10, 10x20", 1),
            ("Hertz Office Space", 3),
            ("10x10 Inside Climate", 5),
        ]);

        let mut result =
            odd_group_names(
                &group_counts,
            );

        result.sort();

        assert_eq!(
            result,
            vec![
                "10x10, 10x20"
                    .to_string(),
                "Hertz Office Space"
                    .to_string(),
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
