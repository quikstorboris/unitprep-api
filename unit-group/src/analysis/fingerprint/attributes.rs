//! Closed-vocabulary structural attributes a UnitGroup name can declare
//! (location, climate, floor access). Each variant's recognized aliases
//! are declared exactly once, in its own ALIASES table, driving both
//! detection (`detect`) and remainder-stripping
//! (`strip_known_attribute_aliases`) — the same literals never need to
//! be maintained in two places, which is how the "Climate matched
//! Non-Climate" class of bug used to happen (a typo in one of the two
//! copies going uncaught).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Inside,
    Outside,
}

impl Location {
    const ALIASES: &'static [(&'static str, Location)] = &[
        ("outside", Location::Outside),
        ("exterior", Location::Outside),
        ("inside", Location::Inside),
        ("interior", Location::Inside),
    ];

    pub fn detect(lower: &str) -> Option<Self> {
        Self::ALIASES
            .iter()
            .find(|(alias, _)| lower.contains(alias))
            .map(|(_, variant)| *variant)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Climate {
    Climate,
    NonClimate,
}

impl Climate {
    const ALIASES: &'static [(&'static str, Climate)] = &[
        ("non-climate", Climate::NonClimate),
        ("non climate", Climate::NonClimate),
        ("climate", Climate::Climate),
    ];

    pub fn detect(lower: &str) -> Option<Self> {
        Self::ALIASES
            .iter()
            .find(|(alias, _)| lower.contains(alias))
            .map(|(_, variant)| *variant)
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
    const ALIASES: &'static [(&'static str, FloorAccess)] = &[
        ("first floor access", FloorAccess::FirstFloorAccess),
        ("ground floor", FloorAccess::GroundFloor),
        ("first floor", FloorAccess::FirstFloor),
        ("second floor", FloorAccess::SecondFloor),
        ("upper level", FloorAccess::UpperLevel),
        ("lower level", FloorAccess::LowerLevel),
    ];

    // Made `pub` to match `Location`/`Climate`'s own `detect` — this was
    // previously the only one of the three left private, an inconsistency
    // noticed while splitting this file out, not a deliberate design
    // choice worth preserving.
    pub fn detect(lower: &str) -> Option<Self> {
        Self::ALIASES
            .iter()
            .find(|(alias, _)| lower.contains(alias))
            .map(|(_, variant)| *variant)
    }
}

/// Strips every known Location/Climate/FloorAccess alias out of `text` —
/// used by `parse_fingerprint` (in `fingerprint::mod`) to compute the
/// "remainder" left over after every recognized attribute is accounted
/// for.
///
/// Correctness here depends on the same alias-ordering requirement each
/// enum's own `detect` relies on: a longer alias that contains a shorter
/// one as a literal substring (e.g. "first floor access" contains "first
/// floor") must be checked/stripped before the shorter one, or the
/// longer alias never fully strips and leaves a dangling remainder token
/// (e.g. "access") behind. See
/// `first_floor_access_does_not_leave_dangling_remainder_token` in
/// `fingerprint::mod`'s own tests for the regression this guards.
pub(super) fn strip_known_attribute_aliases(text: &str) -> String {
    let mut result = text.to_string();

    for (alias, _) in Location::ALIASES {
        result = result.replace(alias, "");
    }

    for (alias, _) in Climate::ALIASES {
        result = result.replace(alias, "");
    }

    for (alias, _) in FloorAccess::ALIASES {
        result = result.replace(alias, "");
    }

    result
}
