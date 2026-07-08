// Fingerprint parsing and matching.
//
// A "fingerprint" is the set of structural business attributes extracted
// from a UnitGroup name (dimensions, location, climate, area code, floor
// access). Two groups are only compared for name similarity if their
// fingerprints match exactly first — this is what keeps fuzzy matching
// from conflating groups that are actually different products (e.g.
// "5x10" vs "7.5x10", or "Inside" vs "Outside").
//
// Every false-positive bug this project has hit so far (dimensions,
// location, climate, area code all matching across genuinely different
// groups) was fixed by adding a new mandatory fingerprint field here —
// keep that pattern: a new business attribute that must never fuzzy-match
// across its own boundary belongs in GroupFingerprint, not in the
// similarity scoring step.
//
// Location/Climate/FloorAccess are closed-vocabulary attributes, so they
// are enums rather than strings: each variant's recognized aliases are
// declared exactly once (in its ALIASES table) and drive both detection
// and remainder-stripping, instead of the same literals being maintained
// separately in two places (which is how the "Climate matched Non-Climate"
// class of bug happens — a typo in one of the two copies goes uncaught).
//
// Width/length/area_code stay as strings: they're open-ended values, not
// a fixed set the compiler can usefully enumerate.

use once_cell::sync::Lazy;
use regex::Regex;

static DIMENSION_REGEX: Lazy<Regex> =
    Lazy::new(|| {
        Regex::new(
            r"(\d+(?:\.\d+)?)\s*x\s*(\d+(?:\.\d+)?)",
        )
        .unwrap()
    });

static AREA_REGEX: Lazy<Regex> =
    Lazy::new(|| {
        Regex::new(
            r"\b([pm]\d+)\b",
        )
        .unwrap()
    });

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Inside,
    Outside,
}

impl Location {
    const ALIASES: &'static [(
        &'static str,
        Location,
    )] = &[
        ("outside", Location::Outside),
        ("exterior", Location::Outside),
        ("inside", Location::Inside),
        ("interior", Location::Inside),
    ];

    pub fn detect(
        lower: &str,
    ) -> Option<Self> {
        Self::ALIASES
            .iter()
            .find(|(alias, _)| {
                lower.contains(alias)
            })
            .map(|(_, variant)| {
                *variant
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Climate {
    Climate,
    NonClimate,
}

impl Climate {
    const ALIASES: &'static [(
        &'static str,
        Climate,
    )] = &[
        (
            "non-climate",
            Climate::NonClimate,
        ),
        (
            "non climate",
            Climate::NonClimate,
        ),
        (
            "climate",
            Climate::Climate,
        ),
    ];

    pub fn detect(
        lower: &str,
    ) -> Option<Self> {
        Self::ALIASES
            .iter()
            .find(|(alias, _)| {
                lower.contains(alias)
            })
            .map(|(_, variant)| {
                *variant
            })
    }
}

// `FirstFloorAccess` deliberately repeats the enum name (clippy flags
// this) — the variant names in this enum mirror the literal business
// term each one represents in the ALIASES table below, so anyone reading
// this can map a variant straight to its alias string. Renaming it to
// avoid the lint would break that 1:1 correspondence for no real benefit.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorAccess {
    FirstFloorAccess,
    GroundFloor,
    FirstFloor,
    SecondFloor,
    UpperLevel,
    LowerLevel,
}

impl FloorAccess {
    const ALIASES: &'static [(
        &'static str,
        FloorAccess,
    )] = &[
        (
            "first floor access",
            FloorAccess::FirstFloorAccess,
        ),
        (
            "ground floor",
            FloorAccess::GroundFloor,
        ),
        (
            "first floor",
            FloorAccess::FirstFloor,
        ),
        (
            "second floor",
            FloorAccess::SecondFloor,
        ),
        (
            "upper level",
            FloorAccess::UpperLevel,
        ),
        (
            "lower level",
            FloorAccess::LowerLevel,
        ),
    ];

    fn detect(
        lower: &str,
    ) -> Option<Self> {
        Self::ALIASES
            .iter()
            .find(|(alias, _)| {
                lower.contains(alias)
            })
            .map(|(_, variant)| {
                *variant
            })
    }
}

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
)]
pub struct GroupFingerprint {
    pub width: Option<String>,
    pub length: Option<String>,
    pub location: Option<Location>,
    pub climate: Option<Climate>,
    pub area_code: Option<String>,
    pub floor_access:
        Option<FloorAccess>,
    pub remainder: String,
}

fn strip_known_attribute_aliases(
    text: &str,
) -> String {
    let mut result =
        text.to_string();

    for (alias, _) in
        Location::ALIASES
    {
        result =
            result.replace(alias, "");
    }

    for (alias, _) in
        Climate::ALIASES
    {
        result =
            result.replace(alias, "");
    }

    for (alias, _) in
        FloorAccess::ALIASES
    {
        result =
            result.replace(alias, "");
    }

    result
}

pub fn parse_fingerprint(
    value: &str,
) -> GroupFingerprint {
    let lower =
        value.to_lowercase();

    let mut width = None;
    let mut length = None;

    if let Some(caps) =
        DIMENSION_REGEX
            .captures(&lower)
    {
        width = caps
            .get(1)
            .map(|m| {
                m.as_str()
                    .to_string()
            });

        length = caps
            .get(2)
            .map(|m| {
                m.as_str()
                    .to_string()
            });
    }

    let location =
        Location::detect(
            &lower,
        );

    let climate =
        Climate::detect(
            &lower,
        );

    let area_code =
        AREA_REGEX
            .captures(&lower)
            .and_then(|caps| {
                caps.get(1)
                    .map(|m| {
                        m.as_str()
                            .to_uppercase()
                    })
            });

    let floor_access =
        FloorAccess::detect(
            &lower,
        );

    let remainder =
        DIMENSION_REGEX
            .replace_all(
                &lower,
                "",
            )
            .to_string();

    let remainder =
        AREA_REGEX
            .replace_all(
                &remainder,
                "",
            )
            .to_string();

    let remainder =
        strip_known_attribute_aliases(
            &remainder,
        )
        .replace("-", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    GroupFingerprint {
        width,
        length,
        location,
        climate,
        area_code,
        floor_access,
        remainder,
    }
}

pub fn fingerprints_match(
    a: &GroupFingerprint,
    b: &GroupFingerprint,
) -> bool {
    if a.width != b.width {
        return false;
    }

    if a.length != b.length {
        return false;
    }

    if a.location
        != b.location
    {
        return false;
    }

    if a.climate
        != b.climate
    {
        return false;
    }

    if a.area_code
        != b.area_code
    {
        return false;
    }

    if a.floor_access
        != b.floor_access
    {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_decimal_sizes_do_not_match() {
        let a =
            parse_fingerprint(
                "5x10 Inside Climate First Floor Access",
            );

        let b =
            parse_fingerprint(
                "7.5x10 Inside Climate First Floor Access",
            );

        assert!(
            !fingerprints_match(
                &a,
                &b,
            )
        );
    }

    #[test]
    fn different_area_codes_do_not_match() {
        let a =
            parse_fingerprint(
                "10x20 Outside Non-Climate P1",
            );

        let b =
            parse_fingerprint(
                "10x20 Outside Non-Climate M2",
            );

        assert!(
            !fingerprints_match(
                &a,
                &b,
            )
        );
    }

    #[test]
    fn inside_and_outside_do_not_match() {
        let a =
            parse_fingerprint(
                "5x10 Inside Non-Climate",
            );

        let b =
            parse_fingerprint(
                "5x10 Outside Non-Climate",
            );

        assert!(
            !fingerprints_match(
                &a,
                &b,
            )
        );
    }

    #[test]
    fn same_fingerprint_matches() {
        let a =
            parse_fingerprint(
                "10x20 Inside Climate First Floor Access",
            );

        let b =
            parse_fingerprint(
                "10x20 Inside Climate First Floor Access",
            );

        assert!(
            fingerprints_match(
                &a,
                &b,
            )
        );
    }

    #[test]
    fn climate_and_non_climate_do_not_match() {
        let a =
            parse_fingerprint(
                "5x10 Inside Climate",
            );

        let b =
            parse_fingerprint(
                "5x10 Inside Non-Climate",
            );

        assert!(
            !fingerprints_match(
                &a,
                &b,
            )
        );
    }

    #[test]
    fn first_floor_access_does_not_leave_dangling_remainder_token() {
        let fp =
            parse_fingerprint(
                "5x10 Inside Climate First Floor Access",
            );

        assert!(
            !fp.remainder
                .contains(
                    "access",
                )
        );
    }
}