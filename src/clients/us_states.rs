//! Normalizes US state values for the clients directory's state filter.
//! `clients.facilities.state` is raw PS free text -- some runs answered
//! it with a two-letter postal abbreviation, others with the full state
//! name (confirmed real inconsistency, not a hypothetical one). Rather
//! than add a CHECK constraint or migrate existing data (which would
//! risk silently dropping a real but unrecognized value), this module
//! canonicalizes at read time: the filter-options list is deduplicated
//! to one full-name entry per state, and selecting that entry expands
//! back to every raw form it could be stored as before querying.

const STATES: &[(&str, &str)] = &[
    ("AL", "Alabama"),
    ("AK", "Alaska"),
    ("AZ", "Arizona"),
    ("AR", "Arkansas"),
    ("CA", "California"),
    ("CO", "Colorado"),
    ("CT", "Connecticut"),
    ("DE", "Delaware"),
    ("DC", "District of Columbia"),
    ("FL", "Florida"),
    ("GA", "Georgia"),
    ("HI", "Hawaii"),
    ("ID", "Idaho"),
    ("IL", "Illinois"),
    ("IN", "Indiana"),
    ("IA", "Iowa"),
    ("KS", "Kansas"),
    ("KY", "Kentucky"),
    ("LA", "Louisiana"),
    ("ME", "Maine"),
    ("MD", "Maryland"),
    ("MA", "Massachusetts"),
    ("MI", "Michigan"),
    ("MN", "Minnesota"),
    ("MS", "Mississippi"),
    ("MO", "Missouri"),
    ("MT", "Montana"),
    ("NE", "Nebraska"),
    ("NV", "Nevada"),
    ("NH", "New Hampshire"),
    ("NJ", "New Jersey"),
    ("NM", "New Mexico"),
    ("NY", "New York"),
    ("NC", "North Carolina"),
    ("ND", "North Dakota"),
    ("OH", "Ohio"),
    ("OK", "Oklahoma"),
    ("OR", "Oregon"),
    ("PA", "Pennsylvania"),
    ("RI", "Rhode Island"),
    ("SC", "South Carolina"),
    ("SD", "South Dakota"),
    ("TN", "Tennessee"),
    ("TX", "Texas"),
    ("UT", "Utah"),
    ("VT", "Vermont"),
    ("VA", "Virginia"),
    ("WA", "Washington"),
    ("WV", "West Virginia"),
    ("WI", "Wisconsin"),
    ("WY", "Wyoming"),
];

/// The canonical full name for a raw state value, matched
/// case-insensitively against either the postal abbreviation or the
/// full name itself. `None` for anything that isn't a recognized US
/// state (e.g. a typo, a Canadian province) -- callers fall back to
/// showing the raw value as-is rather than silently dropping it.
pub fn canonical_name(raw: &str) -> Option<&'static str> {
    let trimmed = raw.trim();
    STATES
        .iter()
        .find(|(abbr, name)| {
            trimmed.eq_ignore_ascii_case(abbr) || trimmed.eq_ignore_ascii_case(name)
        })
        .map(|(_, name)| *name)
}

/// The postal abbreviation for a canonical full name (exact match,
/// case-insensitive) -- `None` if `full_name` isn't one of `STATES`'
/// own full names (e.g. it's an unrecognized raw value being passed
/// through as its own "canonical" name).
pub fn abbreviation_for(full_name: &str) -> Option<&'static str> {
    STATES
        .iter()
        .find(|(_, name)| full_name.eq_ignore_ascii_case(name))
        .map(|(abbr, _)| *abbr)
}

/// Every raw form a canonical full name could be stored as -- itself
/// and its abbreviation, if it's a recognized state -- for expanding a
/// selected filter value back into the `IN (...)` list a raw-text
/// column can actually match. An unrecognized value (not one of
/// `STATES`' full names -- e.g. it round-tripped through
/// `canonical_name` returning `None` and got passed through as-is)
/// expands to just itself.
pub fn raw_variants(canonical_or_raw: &str) -> Vec<String> {
    match abbreviation_for(canonical_or_raw) {
        Some(abbr) => vec![canonical_or_raw.to_string(), abbr.to_string()],
        None => vec![canonical_or_raw.to_string()],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_name_matches_abbreviation_case_insensitively() {
        assert_eq!(canonical_name("ca"), Some("California"));
        assert_eq!(canonical_name("CA"), Some("California"));
    }

    #[test]
    fn canonical_name_matches_full_name_case_insensitively() {
        assert_eq!(canonical_name("california"), Some("California"));
        assert_eq!(canonical_name("California"), Some("California"));
    }

    #[test]
    fn canonical_name_trims_incidental_whitespace() {
        assert_eq!(canonical_name(" CA "), Some("California"));
    }

    #[test]
    fn canonical_name_is_none_for_an_unrecognized_value() {
        assert_eq!(canonical_name("Ontario"), None);
        assert_eq!(canonical_name(""), None);
    }

    #[test]
    fn abbreviation_for_returns_none_for_a_non_state_value() {
        assert_eq!(abbreviation_for("Ontario"), None);
    }

    #[test]
    fn raw_variants_expands_a_recognized_state_to_both_forms() {
        let mut variants = raw_variants("California");
        variants.sort();
        assert_eq!(variants, vec!["CA".to_string(), "California".to_string()]);
    }

    #[test]
    fn raw_variants_passes_through_an_unrecognized_value_unchanged() {
        assert_eq!(raw_variants("Ontario"), vec!["Ontario".to_string()]);
    }
}
